#![allow(dead_code)]

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::gravity::{
    artifact::{PassDirection, QuasiStaticSampleRow, SamplesHeader},
    model::JOINT_COUNT,
    profile::config::SampleReductionMode,
};

pub type ReductionMode = SampleReductionMode;

#[derive(Debug, Clone)]
pub struct SourceSampleArtifact {
    pub source_id: String,
    pub path: PathBuf,
    pub header: SamplesHeader,
    pub rows: Vec<QuasiStaticSampleRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveSampleRow {
    pub source_id: String,
    pub group_id: String,
    pub waypoint_id: u64,
    pub q_rad: [f64; JOINT_COUNT],
    pub dq_rad_s: [f64; JOINT_COUNT],
    pub tau_nm: [f64; JOINT_COUNT],
}

#[derive(Debug, Clone, Copy)]
pub struct ReductionOptions {
    pub pair_q_error_max_rad: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReductionReport {
    pub mode: SampleReductionMode,
    pub raw_sample_count: usize,
    pub effective_sample_count: usize,
    pub paired_waypoint_count: usize,
    pub unpaired_waypoint_count: usize,
    pub skipped_pair_q_error_count: usize,
    pub duplicate_forward_count: usize,
    pub duplicate_backward_count: usize,
    pub pair_q_error_p95_rad: [f64; JOINT_COUNT],
    pub pair_q_error_max_rad: [f64; JOINT_COUNT],
    pub direction_torque_delta_p95_nm: [f64; JOINT_COUNT],
    pub direction_torque_delta_max_nm: [f64; JOINT_COUNT],
}

#[derive(Debug, Clone)]
pub struct ReducedSamples {
    pub header: SamplesHeader,
    pub rows: Vec<EffectiveSampleRow>,
    pub report: ReductionReport,
}

#[derive(Debug, Clone)]
struct DirectionAccumulator {
    count: usize,
    q_sum: [f64; JOINT_COUNT],
    dq_sum: [f64; JOINT_COUNT],
    tau_sum: [f64; JOINT_COUNT],
}

#[derive(Debug, Clone, Default)]
struct WaypointAccumulator {
    forward: Option<DirectionAccumulator>,
    backward: Option<DirectionAccumulator>,
}

#[derive(Debug, Clone)]
struct DirectionMean {
    q_rad: [f64; JOINT_COUNT],
    dq_rad_s: [f64; JOINT_COUNT],
    tau_nm: [f64; JOINT_COUNT],
}

pub fn reduce_samples(
    mode: ReductionMode,
    sources: &[SourceSampleArtifact],
    options: ReductionOptions,
) -> Result<ReducedSamples> {
    match mode {
        ReductionMode::RawRows => reduce_raw_rows(sources),
        ReductionMode::BidirectionalPairMeanV1 => reduce_bidirectional_pair_mean(sources, options),
    }
}

fn reduce_raw_rows(sources: &[SourceSampleArtifact]) -> Result<ReducedSamples> {
    let header = validate_headers_match(sources)?;
    validate_source_artifacts(sources)?;
    validate_source_rows(sources)?;
    let raw_sample_count = raw_sample_count(sources);
    let mut rows = Vec::with_capacity(raw_sample_count);

    for source in sources {
        for row in &source.rows {
            rows.push(EffectiveSampleRow {
                source_id: source.source_id.clone(),
                group_id: raw_row_group_id(row),
                waypoint_id: row.waypoint_id,
                q_rad: row.q_rad,
                dq_rad_s: row.dq_rad_s,
                tau_nm: row.tau_nm,
            });
        }
    }

    let effective_sample_count = rows.len();
    Ok(ReducedSamples {
        header,
        rows,
        report: empty_report(
            ReductionMode::RawRows,
            raw_sample_count,
            effective_sample_count,
        ),
    })
}

fn reduce_bidirectional_pair_mean(
    sources: &[SourceSampleArtifact],
    options: ReductionOptions,
) -> Result<ReducedSamples> {
    if !options.pair_q_error_max_rad.is_finite() || options.pair_q_error_max_rad < 0.0 {
        bail!("pair_q_error_max_rad must be finite and >= 0.0");
    }

    let header = validate_headers_match(sources)?;
    validate_source_artifacts(sources)?;
    validate_source_rows(sources)?;
    let raw_sample_count = raw_sample_count(sources);
    let mut report = empty_report(ReductionMode::BidirectionalPairMeanV1, raw_sample_count, 0);
    let mut grouped = BTreeMap::<(String, u64), WaypointAccumulator>::new();

    for source in sources {
        for row in &source.rows {
            let entry = grouped.entry((source.source_id.clone(), row.waypoint_id)).or_default();
            match row.pass_direction {
                PassDirection::Forward => {
                    if add_direction_row(&mut entry.forward, row) {
                        report.duplicate_forward_count += 1;
                    }
                },
                PassDirection::Backward => {
                    if add_direction_row(&mut entry.backward, row) {
                        report.duplicate_backward_count += 1;
                    }
                },
            }
        }
    }

    let mut rows = Vec::new();
    let mut pair_q_errors = joint_value_buffers();
    let mut direction_torque_deltas = joint_value_buffers();

    for ((source_id, waypoint_id), waypoint) in grouped {
        let (Some(forward), Some(backward)) = (waypoint.forward, waypoint.backward) else {
            report.unpaired_waypoint_count += 1;
            continue;
        };

        let forward = forward.mean(&source_id, waypoint_id, "forward")?;
        let backward = backward.mean(&source_id, waypoint_id, "backward")?;
        let q_error = checked_absolute_delta(
            forward.q_rad,
            backward.q_rad,
            &source_id,
            waypoint_id,
            "q_error",
        )?;

        if max_joint_value(q_error) > options.pair_q_error_max_rad {
            report.skipped_pair_q_error_count += 1;
            continue;
        }

        let torque_delta = checked_absolute_delta(
            forward.tau_nm,
            backward.tau_nm,
            &source_id,
            waypoint_id,
            "torque_delta",
        )?;
        push_joint_values(&mut pair_q_errors, q_error);
        push_joint_values(&mut direction_torque_deltas, torque_delta);

        rows.push(EffectiveSampleRow {
            source_id: source_id.clone(),
            group_id: waypoint_group_id(&source_id, waypoint_id),
            waypoint_id,
            q_rad: checked_average_pair(
                forward.q_rad,
                backward.q_rad,
                &source_id,
                waypoint_id,
                "effective q_rad",
            )?,
            dq_rad_s: checked_average_pair(
                forward.dq_rad_s,
                backward.dq_rad_s,
                &source_id,
                waypoint_id,
                "effective dq_rad_s",
            )?,
            tau_nm: checked_average_pair(
                forward.tau_nm,
                backward.tau_nm,
                &source_id,
                waypoint_id,
                "effective tau_nm",
            )?,
        });
        report.paired_waypoint_count += 1;
    }

    report.effective_sample_count = rows.len();
    report.pair_q_error_p95_rad = percentile_by_joint(&mut pair_q_errors, 0.95);
    report.pair_q_error_max_rad = max_by_joint(&pair_q_errors);
    report.direction_torque_delta_p95_nm = percentile_by_joint(&mut direction_torque_deltas, 0.95);
    report.direction_torque_delta_max_nm = max_by_joint(&direction_torque_deltas);

    Ok(ReducedSamples {
        header,
        rows,
        report,
    })
}

fn validate_headers_match(sources: &[SourceSampleArtifact]) -> Result<SamplesHeader> {
    if sources.is_empty() {
        bail!("expected at least one quasi-static-samples source");
    }

    let first_header = &sources[0].header;
    for source in sources.iter().skip(1) {
        let header = &source.header;
        if first_header.role != header.role {
            bail!(
                "{} role {:?} does not match first artifact role {:?}",
                source.path.display(),
                header.role,
                first_header.role
            );
        }
        if first_header.arm_id != header.arm_id {
            bail!(
                "{} arm_id {:?} does not match first artifact arm_id {:?}",
                source.path.display(),
                header.arm_id,
                first_header.arm_id
            );
        }
        if first_header.joint_map != header.joint_map {
            bail!(
                "{} joint_map {:?} does not match first artifact joint_map {:?}",
                source.path.display(),
                header.joint_map,
                first_header.joint_map
            );
        }
        if first_header.load_profile != header.load_profile {
            bail!(
                "{} load_profile {:?} does not match first artifact load_profile {:?}",
                source.path.display(),
                header.load_profile,
                first_header.load_profile
            );
        }
        if first_header.torque_convention != header.torque_convention {
            bail!(
                "{} torque_convention {:?} does not match first artifact torque_convention {:?}",
                source.path.display(),
                header.torque_convention,
                first_header.torque_convention
            );
        }
    }

    Ok(first_header.clone())
}

fn validate_source_artifacts(sources: &[SourceSampleArtifact]) -> Result<()> {
    let mut seen_source_paths = BTreeMap::<String, PathBuf>::new();

    for source in sources {
        let trimmed_source_id = source.source_id.trim();
        if trimmed_source_id.is_empty() {
            bail!(
                "source_id is blank for artifact path {}",
                source.path.display()
            );
        }

        if let Some(first_path) =
            seen_source_paths.insert(trimmed_source_id.to_string(), source.path.clone())
        {
            bail!(
                "duplicate source_id {:?}: {} and {} use the same source_id",
                trimmed_source_id,
                first_path.display(),
                source.path.display()
            );
        }

        if source.rows.is_empty() {
            bail!(
                "source {} path {} contains no sample rows",
                source.source_id,
                source.path.display()
            );
        }
    }

    Ok(())
}

fn validate_source_rows(sources: &[SourceSampleArtifact]) -> Result<()> {
    for source in sources {
        for (row_index, row) in source.rows.iter().enumerate() {
            validate_joint_values(source, row_index, row, "q_rad", &row.q_rad)?;
            validate_joint_values(source, row_index, row, "dq_rad_s", &row.dq_rad_s)?;
            validate_joint_values(source, row_index, row, "tau_nm", &row.tau_nm)?;
        }
    }

    Ok(())
}

fn validate_joint_values(
    source: &SourceSampleArtifact,
    row_index: usize,
    row: &QuasiStaticSampleRow,
    field_name: &str,
    values: &[f64; JOINT_COUNT],
) -> Result<()> {
    for (joint_index, value) in values.iter().enumerate() {
        if !value.is_finite() {
            bail!(
                "source {} path {} row {} waypoint {} has non-finite {}[{}]: {}",
                source.source_id,
                source.path.display(),
                row_index,
                row.waypoint_id,
                field_name,
                joint_index,
                value
            );
        }
    }

    Ok(())
}

fn raw_sample_count(sources: &[SourceSampleArtifact]) -> usize {
    sources.iter().map(|source| source.rows.len()).sum()
}

fn empty_report(
    mode: ReductionMode,
    raw_sample_count: usize,
    effective_sample_count: usize,
) -> ReductionReport {
    ReductionReport {
        mode,
        raw_sample_count,
        effective_sample_count,
        paired_waypoint_count: 0,
        unpaired_waypoint_count: 0,
        skipped_pair_q_error_count: 0,
        duplicate_forward_count: 0,
        duplicate_backward_count: 0,
        pair_q_error_p95_rad: [0.0; JOINT_COUNT],
        pair_q_error_max_rad: [0.0; JOINT_COUNT],
        direction_torque_delta_p95_nm: [0.0; JOINT_COUNT],
        direction_torque_delta_max_nm: [0.0; JOINT_COUNT],
    }
}

fn add_direction_row(
    direction: &mut Option<DirectionAccumulator>,
    row: &QuasiStaticSampleRow,
) -> bool {
    match direction {
        Some(accumulator) => {
            accumulator.add(row);
            true
        },
        None => {
            *direction = Some(DirectionAccumulator::from_row(row));
            false
        },
    }
}

impl DirectionAccumulator {
    fn from_row(row: &QuasiStaticSampleRow) -> Self {
        Self {
            count: 1,
            q_sum: row.q_rad,
            dq_sum: row.dq_rad_s,
            tau_sum: row.tau_nm,
        }
    }

    fn add(&mut self, row: &QuasiStaticSampleRow) {
        self.count += 1;
        for joint in 0..JOINT_COUNT {
            self.q_sum[joint] += row.q_rad[joint];
            self.dq_sum[joint] += row.dq_rad_s[joint];
            self.tau_sum[joint] += row.tau_nm[joint];
        }
    }

    fn mean(self, source_id: &str, waypoint_id: u64, direction: &str) -> Result<DirectionMean> {
        let count = self.count as f64;
        let q_rad = divide_by(self.q_sum, count);
        let dq_rad_s = divide_by(self.dq_sum, count);
        let tau_nm = divide_by(self.tau_sum, count);

        validate_derived_joint_values(
            source_id,
            waypoint_id,
            &format!("{direction} direction mean q_rad"),
            &q_rad,
        )?;
        validate_derived_joint_values(
            source_id,
            waypoint_id,
            &format!("{direction} direction mean dq_rad_s"),
            &dq_rad_s,
        )?;
        validate_derived_joint_values(
            source_id,
            waypoint_id,
            &format!("{direction} direction mean tau_nm"),
            &tau_nm,
        )?;

        Ok(DirectionMean {
            q_rad,
            dq_rad_s,
            tau_nm,
        })
    }
}

fn raw_row_group_id(row: &QuasiStaticSampleRow) -> String {
    match &row.segment_id {
        Some(segment_id) => format!("segment:{segment_id}"),
        None => format!("waypoint-block:{}", row.waypoint_id / 10),
    }
}

fn waypoint_group_id(source_id: &str, waypoint_id: u64) -> String {
    format!("{source_id}:waypoint:{waypoint_id}")
}

fn divide_by(mut values: [f64; JOINT_COUNT], divisor: f64) -> [f64; JOINT_COUNT] {
    for value in &mut values {
        *value /= divisor;
    }
    values
}

fn checked_average_pair(
    left: [f64; JOINT_COUNT],
    right: [f64; JOINT_COUNT],
    source_id: &str,
    waypoint_id: u64,
    field_name: &str,
) -> Result<[f64; JOINT_COUNT]> {
    let mut average = [0.0; JOINT_COUNT];
    for joint in 0..JOINT_COUNT {
        average[joint] = (left[joint] + right[joint]) * 0.5;
    }
    validate_derived_joint_values(source_id, waypoint_id, field_name, &average)?;
    Ok(average)
}

fn checked_absolute_delta(
    left: [f64; JOINT_COUNT],
    right: [f64; JOINT_COUNT],
    source_id: &str,
    waypoint_id: u64,
    field_name: &str,
) -> Result<[f64; JOINT_COUNT]> {
    let mut delta = [0.0; JOINT_COUNT];
    for joint in 0..JOINT_COUNT {
        delta[joint] = (left[joint] - right[joint]).abs();
    }
    validate_derived_joint_values(source_id, waypoint_id, field_name, &delta)?;
    Ok(delta)
}

fn validate_derived_joint_values(
    source_id: &str,
    waypoint_id: u64,
    field_name: &str,
    values: &[f64; JOINT_COUNT],
) -> Result<()> {
    for (joint_index, value) in values.iter().enumerate() {
        if !value.is_finite() {
            bail!(
                "source {} waypoint {} produced non-finite {}[{}]: {}",
                source_id,
                waypoint_id,
                field_name,
                joint_index,
                value
            );
        }
    }

    Ok(())
}

fn max_joint_value(values: [f64; JOINT_COUNT]) -> f64 {
    values.into_iter().fold(0.0, f64::max)
}

fn joint_value_buffers() -> Vec<Vec<f64>> {
    (0..JOINT_COUNT).map(|_| Vec::new()).collect()
}

fn push_joint_values(buffers: &mut [Vec<f64>], values: [f64; JOINT_COUNT]) {
    for joint in 0..JOINT_COUNT {
        buffers[joint].push(values[joint]);
    }
}

fn percentile_by_joint(buffers: &mut [Vec<f64>], quantile: f64) -> [f64; JOINT_COUNT] {
    let mut percentiles = [0.0; JOINT_COUNT];
    for joint in 0..JOINT_COUNT {
        buffers[joint].sort_by(|left, right| {
            left.partial_cmp(right).expect("sample reduction metrics are finite")
        });
        percentiles[joint] = percentile_from_sorted(&buffers[joint], quantile);
    }
    percentiles
}

fn max_by_joint(buffers: &[Vec<f64>]) -> [f64; JOINT_COUNT] {
    let mut maximums = [0.0; JOINT_COUNT];
    for joint in 0..JOINT_COUNT {
        maximums[joint] = buffers[joint].iter().copied().fold(0.0, f64::max);
    }
    maximums
}

fn percentile_from_sorted(values: &[f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[index]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::gravity::artifact::{PassDirection, QuasiStaticSampleRow, SamplesHeader};

    #[test]
    fn pair_mean_averages_forward_and_backward_rows() {
        let source = source_for_tests(
            "samples-a",
            vec![
                row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]),
                row_for_tests(7, PassDirection::Backward, [3.0; 6], [14.0; 6]),
            ],
        );

        let reduced = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 3.0,
            },
        )
        .unwrap();

        assert_eq!(reduced.rows.len(), 1);
        assert_eq!(reduced.rows[0].source_id, "samples-a");
        assert_eq!(reduced.rows[0].group_id, "samples-a:waypoint:7");
        assert_eq!(reduced.rows[0].waypoint_id, 7);
        assert_eq!(reduced.rows[0].q_rad, [2.0; 6]);
        assert_eq!(reduced.rows[0].tau_nm, [12.0; 6]);
        assert_eq!(reduced.report.paired_waypoint_count, 1);
    }

    #[test]
    fn raw_rows_use_legacy_segment_group_id() {
        let mut row = row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]);
        row.segment_id = Some("seg-a".to_string());
        let source = source_for_tests("samples-a", vec![row]);

        let reduced = reduce_samples(
            ReductionMode::RawRows,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.0,
            },
        )
        .unwrap();

        assert_eq!(reduced.rows[0].group_id, "segment:seg-a");
    }

    #[test]
    fn raw_rows_use_legacy_waypoint_block_group_id_without_segment() {
        let source = source_for_tests(
            "samples-a",
            vec![row_for_tests(
                27,
                PassDirection::Forward,
                [1.0; 6],
                [10.0; 6],
            )],
        );

        let reduced = reduce_samples(
            ReductionMode::RawRows,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.0,
            },
        )
        .unwrap();

        assert_eq!(reduced.rows[0].group_id, "waypoint-block:2");
    }

    #[test]
    fn reducer_does_not_pair_rows_from_different_sources_with_same_waypoint_id() {
        let sources = vec![
            source_for_tests(
                "samples-a",
                vec![row_for_tests(
                    7,
                    PassDirection::Forward,
                    [1.0; 6],
                    [10.0; 6],
                )],
            ),
            source_for_tests(
                "samples-b",
                vec![row_for_tests(
                    7,
                    PassDirection::Backward,
                    [3.0; 6],
                    [14.0; 6],
                )],
            ),
        ];

        let reduced = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &sources,
            ReductionOptions {
                pair_q_error_max_rad: 3.0,
            },
        )
        .unwrap();

        assert!(reduced.rows.is_empty());
        assert_eq!(reduced.report.unpaired_waypoint_count, 2);
    }

    #[test]
    fn reducer_does_not_pair_only_by_segment_id() {
        let mut forward = row_for_tests(1, PassDirection::Forward, [1.0; 6], [10.0; 6]);
        forward.segment_id = Some("segment-a".to_string());
        let mut backward = row_for_tests(2, PassDirection::Backward, [1.0; 6], [12.0; 6]);
        backward.segment_id = Some("segment-a".to_string());

        let source = source_for_tests("samples-a", vec![forward, backward]);
        let reduced = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.05,
            },
        )
        .unwrap();

        assert!(reduced.rows.is_empty());
        assert_eq!(reduced.report.unpaired_waypoint_count, 2);
    }

    #[test]
    fn reducer_skips_pairs_with_large_q_error() {
        let source = source_for_tests(
            "samples-a",
            vec![
                row_for_tests(7, PassDirection::Forward, [0.0; 6], [10.0; 6]),
                row_for_tests(7, PassDirection::Backward, [0.2; 6], [14.0; 6]),
            ],
        );

        let reduced = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.05,
            },
        )
        .unwrap();

        assert!(reduced.rows.is_empty());
        assert_eq!(reduced.report.skipped_pair_q_error_count, 1);
    }

    #[test]
    fn reducer_reports_metrics_only_for_pairs_accepted_by_q_error_filter() {
        let source = source_for_tests(
            "samples-a",
            vec![
                row_for_tests(7, PassDirection::Forward, [0.0; 6], [1.0; 6]),
                row_for_tests(7, PassDirection::Backward, [0.02; 6], [1.5; 6]),
                row_for_tests(8, PassDirection::Forward, [0.0; 6], [10.0; 6]),
                row_for_tests(8, PassDirection::Backward, [0.5; 6], [30.0; 6]),
            ],
        );

        let reduced = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.05,
            },
        )
        .unwrap();

        assert_eq!(reduced.rows.len(), 1);
        assert_eq!(reduced.report.skipped_pair_q_error_count, 1);
        assert_eq!(reduced.report.pair_q_error_max_rad, [0.02; 6]);
        assert_eq!(reduced.report.direction_torque_delta_max_nm, [0.5; 6]);
    }

    #[test]
    fn reducer_averages_duplicate_rows_per_direction_before_pair_mean() {
        let source = source_for_tests(
            "samples-a",
            vec![
                row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]),
                row_for_tests(7, PassDirection::Forward, [3.0; 6], [14.0; 6]),
                row_for_tests(7, PassDirection::Backward, [5.0; 6], [18.0; 6]),
                row_for_tests(7, PassDirection::Backward, [7.0; 6], [22.0; 6]),
            ],
        );

        let reduced = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 4.0,
            },
        )
        .unwrap();

        assert_eq!(reduced.rows.len(), 1);
        assert_eq!(reduced.rows[0].q_rad, [4.0; 6]);
        assert_eq!(reduced.rows[0].tau_nm, [16.0; 6]);
        assert_eq!(reduced.report.duplicate_forward_count, 1);
        assert_eq!(reduced.report.duplicate_backward_count, 1);
    }

    #[test]
    fn pair_mean_returns_err_when_q_value_is_nan() {
        let mut forward = row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]);
        forward.q_rad[0] = f64::NAN;
        let source = source_for_tests(
            "samples-a",
            vec![
                forward,
                row_for_tests(7, PassDirection::Backward, [1.0; 6], [10.0; 6]),
            ],
        );

        let err = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 1.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("samples-a"));
        assert!(message.contains("samples-a.jsonl"));
        assert!(message.contains("row 0"));
        assert!(message.contains("q_rad[0]"));
    }

    #[test]
    fn raw_rows_returns_err_when_tau_value_is_nan() {
        let mut row = row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]);
        row.tau_nm[0] = f64::NAN;
        let source = source_for_tests("samples-a", vec![row]);

        let err = reduce_samples(
            ReductionMode::RawRows,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("samples-a"));
        assert!(message.contains("samples-a.jsonl"));
        assert!(message.contains("row 0"));
        assert!(message.contains("tau_nm[0]"));
    }

    #[test]
    fn raw_rows_returns_err_when_dq_value_is_nan() {
        let mut row = row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]);
        row.dq_rad_s[0] = f64::NAN;
        let source = source_for_tests("samples-a", vec![row]);

        let err = reduce_samples(
            ReductionMode::RawRows,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("samples-a"));
        assert!(message.contains("samples-a.jsonl"));
        assert!(message.contains("row 0"));
        assert!(message.contains("dq_rad_s[0]"));
    }

    #[test]
    fn duplicate_source_ids_are_rejected() {
        let sources = vec![
            source_for_tests(
                "samples-a",
                vec![row_for_tests(
                    7,
                    PassDirection::Forward,
                    [1.0; 6],
                    [10.0; 6],
                )],
            ),
            source_for_tests(
                "samples-a",
                vec![row_for_tests(
                    7,
                    PassDirection::Backward,
                    [1.0; 6],
                    [10.0; 6],
                )],
            ),
        ];

        let err = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &sources,
            ReductionOptions {
                pair_q_error_max_rad: 1.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("duplicate source_id"));
        assert!(message.contains("samples-a"));
        assert!(message.contains("samples-a.jsonl"));
    }

    #[test]
    fn blank_source_id_is_rejected() {
        let source = source_for_tests(
            "   ",
            vec![row_for_tests(
                7,
                PassDirection::Forward,
                [1.0; 6],
                [10.0; 6],
            )],
        );

        let err = reduce_samples(
            ReductionMode::RawRows,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("source_id"));
        assert!(message.contains("blank"));
    }

    #[test]
    fn empty_raw_rows_source_is_rejected() {
        let source = source_for_tests("samples-a", Vec::new());

        let err = reduce_samples(
            ReductionMode::RawRows,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("samples-a"));
        assert!(message.contains("samples-a.jsonl"));
        assert!(message.contains("contains no sample rows"));
    }

    #[test]
    fn empty_pair_mean_source_is_rejected() {
        let source = source_for_tests("samples-a", Vec::new());

        let err = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 1.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("samples-a"));
        assert!(message.contains("samples-a.jsonl"));
        assert!(message.contains("contains no sample rows"));
    }

    #[test]
    fn invalid_pair_q_error_max_rad_is_rejected() {
        let source = source_for_tests(
            "samples-a",
            vec![
                row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]),
                row_for_tests(7, PassDirection::Backward, [1.0; 6], [10.0; 6]),
            ],
        );

        let err = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: f64::NAN,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("pair_q_error_max_rad must be finite and >= 0.0"));
    }

    #[test]
    fn header_mismatch_is_rejected() {
        let first = source_for_tests(
            "samples-a",
            vec![row_for_tests(
                7,
                PassDirection::Forward,
                [1.0; 6],
                [10.0; 6],
            )],
        );
        let mut second = source_for_tests(
            "samples-b",
            vec![row_for_tests(
                8,
                PassDirection::Forward,
                [1.0; 6],
                [10.0; 6],
            )],
        );
        second.header.load_profile = "loaded".to_string();

        let err = reduce_samples(
            ReductionMode::RawRows,
            &[first, second],
            ReductionOptions {
                pair_q_error_max_rad: 0.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("samples-b.jsonl"));
        assert!(message.contains("load_profile"));
    }

    #[test]
    fn pair_mean_returns_err_when_direction_mean_overflows() {
        let source = source_for_tests(
            "samples-a",
            vec![
                row_for_tests(7, PassDirection::Forward, [f64::MAX; 6], [10.0; 6]),
                row_for_tests(7, PassDirection::Forward, [f64::MAX; 6], [10.0; 6]),
                row_for_tests(7, PassDirection::Backward, [1.0; 6], [10.0; 6]),
            ],
        );

        let err = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: f64::MAX,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("non-finite"));
        assert!(message.contains("direction mean q_rad[0]"));
    }

    #[test]
    fn pair_mean_returns_err_when_q_error_overflows() {
        let source = source_for_tests(
            "samples-a",
            vec![
                row_for_tests(7, PassDirection::Forward, [f64::MAX; 6], [10.0; 6]),
                row_for_tests(7, PassDirection::Backward, [-f64::MAX; 6], [10.0; 6]),
            ],
        );

        let err = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: f64::MAX,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("non-finite"));
        assert!(message.contains("q_error[0]"));
    }

    #[test]
    fn pair_mean_returns_err_when_effective_average_overflows() {
        let source = source_for_tests(
            "samples-a",
            vec![
                row_for_tests(7, PassDirection::Forward, [f64::MAX; 6], [10.0; 6]),
                row_for_tests(7, PassDirection::Backward, [f64::MAX; 6], [10.0; 6]),
            ],
        );

        let err = reduce_samples(
            ReductionMode::BidirectionalPairMeanV1,
            &[source],
            ReductionOptions {
                pair_q_error_max_rad: 0.0,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("non-finite"));
        assert!(message.contains("effective q_rad[0]"));
    }

    fn source_for_tests(
        source_id: impl Into<String>,
        rows: Vec<QuasiStaticSampleRow>,
    ) -> SourceSampleArtifact {
        let source_id = source_id.into();
        SourceSampleArtifact {
            path: PathBuf::from(format!("{source_id}.jsonl")),
            source_id,
            header: sample_header_for_tests(),
            rows,
        }
    }

    fn row_for_tests(
        waypoint_id: u64,
        pass_direction: PassDirection,
        q_rad: [f64; 6],
        tau_nm: [f64; 6],
    ) -> QuasiStaticSampleRow {
        QuasiStaticSampleRow {
            row_type: "quasi-static-sample".to_string(),
            waypoint_id,
            segment_id: None,
            pass_direction,
            host_mono_us: 1,
            raw_timestamp_us: None,
            q_rad,
            dq_rad_s: [0.0; 6],
            tau_nm,
            position_valid_mask: 0x3f,
            dynamic_valid_mask: 0x3f,
            stable_velocity_rad_s: 0.0,
            stable_tracking_error_rad: 0.0,
            stable_torque_std_nm: 0.0,
        }
    }

    fn sample_header_for_tests() -> SamplesHeader {
        SamplesHeader {
            row_type: "header".to_string(),
            artifact_kind: "quasi-static-samples".to_string(),
            schema_version: 1,
            source_path: "synthetic".to_string(),
            source_sha256: "synthetic".to_string(),
            role: "slave".to_string(),
            arm_id: None,
            target: "synthetic".to_string(),
            joint_map: "piper_default".to_string(),
            load_profile: "unloaded".to_string(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            frequency_hz: 100.0,
            max_velocity_rad_s: 0.08,
            max_step_rad: 0.02,
            settle_ms: 500,
            sample_ms: 300,
            stable_velocity_rad_s: 0.01,
            stable_tracking_error_rad: 0.03,
            stable_torque_std_nm: 0.08,
            waypoint_count: 600,
            accepted_waypoint_count: 600,
            rejected_waypoint_count: 0,
        }
    }
}
