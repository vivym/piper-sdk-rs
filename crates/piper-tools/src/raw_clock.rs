use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawClockSample {
    pub raw_us: u64,
    /// Timestamp on the common clock used as the fit target. Existing callers
    /// may pass their receive monotonic timestamp here. SocketCAN raw-clock
    /// callers should prefer kernel timestamps and pass receive monotonic time
    /// separately via `push_with_receive_mono_us`.
    pub host_rx_mono_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RawClockThresholds {
    pub warmup_samples: usize,
    pub warmup_window_us: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_us: u64,
    pub last_sample_age_us: u64,
}

#[cfg(test)]
impl RawClockThresholds {
    const fn for_tests() -> Self {
        Self {
            warmup_samples: 4,
            warmup_window_us: 3_000,
            residual_p95_us: 100,
            residual_max_us: 250,
            drift_abs_ppm: 100.0,
            sample_gap_max_us: 10_000,
            last_sample_age_us: 2_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawClockHealth {
    pub healthy: bool,
    pub sample_count: usize,
    pub window_duration_us: u64,
    pub drift_ppm: f64,
    pub residual_p50_us: u64,
    pub residual_p95_us: u64,
    pub residual_p99_us: u64,
    pub residual_max_us: u64,
    pub sample_gap_max_us: u64,
    pub last_sample_age_us: u64,
    pub raw_timestamp_regressions: u64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawClockError {
    RawTimestampRegression { previous_raw_us: u64, raw_us: u64 },
}

impl fmt::Display for RawClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RawTimestampRegression {
                previous_raw_us,
                raw_us,
            } => write!(
                f,
                "raw timestamp regression: previous_raw_us={previous_raw_us}, raw_us={raw_us}"
            ),
        }
    }
}

impl Error for RawClockError {}

pub struct RawClockEstimator {
    thresholds: RawClockThresholds,
    samples: VecDeque<TimedRawClockSample>,
    raw_timestamp_regressions: u64,
    slope: Option<f64>,
    offset: Option<f64>,
    continuity_boundary_raw_us: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimedRawClockSample {
    raw_us: u64,
    host_rx_mono_us: u64,
    receive_mono_us: u64,
}

impl TimedRawClockSample {
    const fn new(sample: RawClockSample, receive_mono_us: u64) -> Self {
        Self {
            raw_us: sample.raw_us,
            host_rx_mono_us: sample.host_rx_mono_us,
            receive_mono_us,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HealthMetrics {
    fit_ready: bool,
    sample_count: usize,
    window_duration_us: u64,
    drift_ppm: f64,
    residual_p95_us: u64,
    residual_max_us: u64,
    sample_gap_max_us: u64,
    last_sample_age_us: u64,
}

impl RawClockEstimator {
    pub fn new(thresholds: RawClockThresholds) -> Self {
        Self {
            thresholds,
            samples: VecDeque::new(),
            raw_timestamp_regressions: 0,
            slope: None,
            offset: None,
            continuity_boundary_raw_us: None,
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.raw_timestamp_regressions = 0;
        self.slope = None;
        self.offset = None;
        self.continuity_boundary_raw_us = None;
    }

    pub fn mark_continuity_boundary(&mut self) {
        self.continuity_boundary_raw_us = self.samples.back().map(|sample| sample.raw_us);
    }

    pub fn push(&mut self, sample: RawClockSample) -> Result<(), RawClockError> {
        self.push_with_receive_mono_us(sample, sample.host_rx_mono_us)
    }

    pub fn push_with_receive_mono_us(
        &mut self,
        sample: RawClockSample,
        receive_mono_us: u64,
    ) -> Result<(), RawClockError> {
        let sample = TimedRawClockSample::new(sample, receive_mono_us);
        if let Some(previous) = self.samples.back()
            && sample.raw_us <= previous.raw_us
        {
            self.raw_timestamp_regressions = self.raw_timestamp_regressions.saturating_add(1);
            return Err(RawClockError::RawTimestampRegression {
                previous_raw_us: previous.raw_us,
                raw_us: sample.raw_us,
            });
        }

        self.samples.push_back(sample);
        self.prune_samples(sample.host_rx_mono_us);
        self.recompute_fit();
        Ok(())
    }

    pub fn map_raw_us(&self, raw_us: u64) -> Option<u64> {
        let slope = self.slope?;
        let offset = self.offset?;
        let host_us = slope.mul_add(raw_us as f64, offset);
        if !host_us.is_finite() {
            return None;
        }

        Some(host_us.round().clamp(0.0, u64::MAX as f64) as u64)
    }

    pub fn health(&self, now_host_us: u64) -> RawClockHealth {
        let fit_ready = self.slope.is_some() && self.offset.is_some();
        let sample_count = self.samples.len();
        let window_duration_us = self.window_duration_us();
        let sample_gap_max_us = self.sample_gap_max_us();
        let last_sample_age_us = self
            .samples
            .back()
            .map(|sample| now_host_us.saturating_sub(sample.receive_mono_us))
            .unwrap_or(u64::MAX);

        let mut residuals = self.residuals();
        residuals.sort_unstable();
        let residual_p50_us = percentile_sorted(&residuals, 50);
        let residual_p95_us = percentile_sorted(&residuals, 95);
        let residual_p99_us = percentile_sorted(&residuals, 99);
        let residual_max_us = residuals.last().copied().unwrap_or(0);
        let drift_ppm = self.drift_ppm();

        let metrics = HealthMetrics {
            fit_ready,
            sample_count,
            window_duration_us,
            drift_ppm,
            residual_p95_us,
            residual_max_us,
            sample_gap_max_us,
            last_sample_age_us,
        };
        let reason = self.unhealthy_reason(metrics);

        RawClockHealth {
            healthy: reason.is_none(),
            sample_count,
            window_duration_us,
            drift_ppm,
            residual_p50_us,
            residual_p95_us,
            residual_p99_us,
            residual_max_us,
            sample_gap_max_us,
            last_sample_age_us,
            raw_timestamp_regressions: self.raw_timestamp_regressions,
            reason,
        }
    }

    fn prune_samples(&mut self, newest_host_us: u64) {
        let retention_window_us = self
            .thresholds
            .warmup_window_us
            .saturating_mul(2)
            .saturating_add(self.thresholds.sample_gap_max_us)
            .max(self.thresholds.sample_gap_max_us.saturating_mul(4));
        while self.samples.front().is_some_and(|sample| {
            newest_host_us.saturating_sub(sample.host_rx_mono_us) > retention_window_us
        }) {
            self.samples.pop_front();
        }
    }

    fn recompute_fit(&mut self) {
        self.slope = None;
        self.offset = None;

        let selected = self.filtered_lower_envelope_samples();
        let Some((slope, offset)) = fit_line(&selected) else {
            return;
        };

        self.slope = Some(slope);
        self.offset = Some(offset);
    }

    fn filtered_lower_envelope_samples(&self) -> Vec<TimedRawClockSample> {
        let Some(first_raw_us) = self.samples.front().map(|sample| sample.raw_us) else {
            return Vec::new();
        };
        let bucket_width_us = (self.thresholds.warmup_window_us
            / self.thresholds.warmup_samples.max(1) as u64)
            .max(1);
        let mut bucketed: Vec<(u64, TimedRawClockSample)> = Vec::new();

        for sample in &self.samples {
            let bucket = sample.raw_us.saturating_sub(first_raw_us) / bucket_width_us;
            let delay_score = sample.host_rx_mono_us.saturating_sub(sample.raw_us);

            if let Some((_, existing)) =
                bucketed.iter_mut().find(|(existing_bucket, _)| *existing_bucket == bucket)
            {
                let existing_delay_score = existing.host_rx_mono_us.saturating_sub(existing.raw_us);
                if delay_score < existing_delay_score
                    || (delay_score == existing_delay_score
                        && sample.host_rx_mono_us < existing.host_rx_mono_us)
                {
                    *existing = *sample;
                }
            } else {
                bucketed.push((bucket, *sample));
            }
        }

        let mut selected: Vec<_> = bucketed.into_iter().map(|(_, sample)| sample).collect();
        selected.sort_unstable_by_key(|sample| sample.raw_us);
        drop_high_receive_delay_outliers(&selected, self.thresholds)
    }

    fn residuals(&self) -> Vec<u64> {
        self.samples
            .iter()
            .filter_map(|sample| {
                let mapped = self.map_raw_us(sample.raw_us)?;
                Some(mapped.abs_diff(sample.host_rx_mono_us))
            })
            .collect()
    }

    fn window_duration_us(&self) -> u64 {
        match (self.samples.front(), self.samples.back()) {
            (Some(first), Some(last)) => last.host_rx_mono_us.saturating_sub(first.host_rx_mono_us),
            _ => 0,
        }
    }

    fn sample_gap_max_us(&self) -> u64 {
        self.samples
            .iter()
            .zip(self.samples.iter().skip(1))
            .filter(|(previous, sample)| {
                !self
                    .continuity_boundary_raw_us
                    .is_some_and(|boundary| previous.raw_us <= boundary && sample.raw_us > boundary)
            })
            .map(|(previous, sample)| sample.raw_us.saturating_sub(previous.raw_us))
            .max()
            .unwrap_or(0)
    }

    fn drift_ppm(&self) -> f64 {
        let Some(slope) = self.slope else {
            return 0.0;
        };

        (slope - 1.0) * 1_000_000.0
    }

    fn unhealthy_reason(&self, metrics: HealthMetrics) -> Option<String> {
        if !metrics.fit_ready {
            return Some("line fit unavailable".to_string());
        }
        if metrics.sample_count < self.thresholds.warmup_samples {
            return Some(format!(
                "sample count {} below warmup threshold {}",
                metrics.sample_count, self.thresholds.warmup_samples
            ));
        }
        if metrics.window_duration_us < self.thresholds.warmup_window_us {
            return Some(format!(
                "window duration {}us below warmup threshold {}us",
                metrics.window_duration_us, self.thresholds.warmup_window_us
            ));
        }
        if metrics.residual_p95_us > self.thresholds.residual_p95_us {
            return Some(format!(
                "residual p95 {}us exceeds threshold {}us",
                metrics.residual_p95_us, self.thresholds.residual_p95_us
            ));
        }
        if metrics.residual_max_us > self.thresholds.residual_max_us {
            return Some(format!(
                "residual max {}us exceeds threshold {}us",
                metrics.residual_max_us, self.thresholds.residual_max_us
            ));
        }
        if metrics.drift_ppm.abs() > self.thresholds.drift_abs_ppm {
            return Some(format!(
                "drift {:.3}ppm exceeds threshold {:.3}ppm",
                metrics.drift_ppm, self.thresholds.drift_abs_ppm
            ));
        }
        if metrics.sample_gap_max_us > self.thresholds.sample_gap_max_us {
            return Some(format!(
                "sample gap {}us exceeds threshold {}us",
                metrics.sample_gap_max_us, self.thresholds.sample_gap_max_us
            ));
        }
        if metrics.last_sample_age_us > self.thresholds.last_sample_age_us {
            return Some(format!(
                "last sample age {}us exceeds threshold {}us",
                metrics.last_sample_age_us, self.thresholds.last_sample_age_us
            ));
        }
        if self.raw_timestamp_regressions > 0 {
            return Some(format!(
                "raw timestamp regressions observed: {}",
                self.raw_timestamp_regressions
            ));
        }
        None
    }
}

fn drop_high_receive_delay_outliers(
    selected: &[TimedRawClockSample],
    thresholds: RawClockThresholds,
) -> Vec<TimedRawClockSample> {
    let fit_outlier_us = thresholds.residual_p95_us.min(thresholds.residual_max_us).max(50) as f64;

    selected
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| {
            let (Some(before), Some(after)) = (
                index.checked_sub(1).and_then(|i| selected.get(i)),
                selected.get(index + 1),
            ) else {
                return Some(*sample);
            };

            let raw_span_us = after.raw_us.saturating_sub(before.raw_us);
            if raw_span_us == 0 {
                return Some(*sample);
            }

            let raw_offset_us = sample.raw_us.saturating_sub(before.raw_us);
            let fraction = raw_offset_us as f64 / raw_span_us as f64;
            let host_span_us = after.host_rx_mono_us as f64 - before.host_rx_mono_us as f64;
            let interpolated_host_us = before.host_rx_mono_us as f64 + fraction * host_span_us;

            if sample.host_rx_mono_us as f64 > interpolated_host_us + fit_outlier_us {
                None
            } else {
                Some(*sample)
            }
        })
        .collect()
}

fn fit_line(selected: &[TimedRawClockSample]) -> Option<(f64, f64)> {
    if selected.len() < 2 {
        return None;
    }

    let raw_mean = selected.iter().map(|s| s.raw_us as f64).sum::<f64>() / selected.len() as f64;
    let host_mean =
        selected.iter().map(|s| s.host_rx_mono_us as f64).sum::<f64>() / selected.len() as f64;
    let variance = selected
        .iter()
        .map(|s| {
            let dr = s.raw_us as f64 - raw_mean;
            dr * dr
        })
        .sum::<f64>();
    if variance == 0.0 {
        return None;
    }
    let covariance = selected
        .iter()
        .map(|s| {
            let dr = s.raw_us as f64 - raw_mean;
            let dh = s.host_rx_mono_us as f64 - host_mean;
            dr * dh
        })
        .sum::<f64>();
    let slope = covariance / variance;
    let offset = host_mean - slope * raw_mean;

    if slope.is_finite() && offset.is_finite() {
        Some((slope, offset))
    } else {
        None
    }
}

fn percentile_sorted(sorted: &[u64], percentile: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }

    let rank = (percentile as usize * (sorted.len() - 1)).div_ceil(100);
    sorted[rank.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(raw_us: u64, host_us: u64) -> RawClockSample {
        RawClockSample {
            raw_us,
            host_rx_mono_us: host_us,
        }
    }

    #[test]
    fn stable_clock_becomes_healthy_and_maps_raw_to_host() {
        let thresholds = RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 3_000,
            residual_p95_us: 20,
            residual_max_us: 50,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 2_000,
            last_sample_age_us: 2_000,
        };
        let mut estimator = RawClockEstimator::new(thresholds);

        estimator.push(sample(10_000, 110_000)).unwrap();
        estimator.push(sample(11_000, 111_003)).unwrap();
        estimator.push(sample(12_000, 112_001)).unwrap();
        estimator.push(sample(13_000, 113_002)).unwrap();

        let health = estimator.health(113_100);
        assert!(health.healthy, "{health:?}");
        assert!((estimator.map_raw_us(12_500).unwrap() as i64 - 112_500).abs() <= 10);
    }

    #[test]
    fn raw_timestamp_regression_fails_closed() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds::for_tests());
        estimator.push(sample(10_000, 110_000)).unwrap();

        let err = estimator.push(sample(9_999, 110_100)).unwrap_err();
        assert!(matches!(err, RawClockError::RawTimestampRegression { .. }));
        assert!(!estimator.health(110_100).healthy);
    }

    #[test]
    fn reset_clears_samples_fit_and_regression_state() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds::for_tests());
        estimator.push(sample(10_000, 110_000)).unwrap();
        let err = estimator.push(sample(9_999, 110_100)).unwrap_err();
        assert!(matches!(err, RawClockError::RawTimestampRegression { .. }));

        estimator.reset();

        let health = estimator.health(110_200);
        assert_eq!(health.sample_count, 0);
        assert_eq!(health.raw_timestamp_regressions, 0);
        assert!(estimator.map_raw_us(10_000).is_none());
        estimator.push(sample(10_000, 110_300)).unwrap();
    }

    #[test]
    fn continuity_boundary_ignores_single_cross_boundary_sample_gap() {
        let thresholds = RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 12_000,
            residual_p95_us: 20,
            residual_max_us: 50,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 2_000,
            last_sample_age_us: 2_000,
        };
        let mut estimator = RawClockEstimator::new(thresholds);

        estimator.push(sample(10_000, 110_000)).unwrap();
        estimator.push(sample(11_000, 111_000)).unwrap();
        estimator.push(sample(12_000, 112_000)).unwrap();
        estimator.push(sample(13_000, 113_000)).unwrap();
        estimator.mark_continuity_boundary();
        estimator.push(sample(20_000, 120_000)).unwrap();
        estimator.push(sample(21_000, 121_000)).unwrap();
        estimator.push(sample(22_000, 122_000)).unwrap();
        estimator.push(sample(23_000, 123_000)).unwrap();

        let health = estimator.health(123_100);
        assert!(health.healthy, "{health:?}");
        assert_eq!(health.sample_gap_max_us, 1_000);
    }

    #[test]
    fn one_sample_without_fit_fails_closed_even_with_permissive_thresholds() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds {
            warmup_samples: 1,
            warmup_window_us: 0,
            residual_p95_us: u64::MAX,
            residual_max_us: u64::MAX,
            drift_abs_ppm: f64::MAX,
            sample_gap_max_us: u64::MAX,
            last_sample_age_us: u64::MAX,
        });

        estimator.push(sample(10_000, 110_000)).unwrap();

        assert!(estimator.map_raw_us(10_000).is_none());
        let health = estimator.health(110_000);
        assert!(!health.healthy, "{health:?}");
        assert!(
            health.reason.as_deref().is_some_and(|reason| reason.contains("fit")),
            "{health:?}"
        );
    }

    #[test]
    fn excessive_residual_marks_unhealthy() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds {
            residual_p95_us: 50,
            residual_max_us: 100,
            ..RawClockThresholds::for_tests()
        });

        for i in 0..8 {
            estimator.push(sample(10_000 + i * 1_000, 110_000 + i * 1_000)).unwrap();
        }
        estimator.push(sample(19_000, 120_000)).unwrap();

        let health = estimator.health(120_000);
        assert!(!health.healthy);
        assert!(health.residual_max_us > 100);
    }

    #[test]
    fn excessive_drift_marks_unhealthy() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds {
            drift_abs_ppm: 10.0,
            ..RawClockThresholds::for_tests()
        });

        estimator.push(sample(10_000, 110_000)).unwrap();
        estimator.push(sample(20_000, 120_500)).unwrap();
        estimator.push(sample(30_000, 131_000)).unwrap();
        estimator.push(sample(40_000, 141_500)).unwrap();

        let health = estimator.health(141_500);
        assert!(!health.healthy);
        assert!(health.drift_ppm.abs() > 10.0);
    }

    #[test]
    fn fitted_drift_is_not_discounted_by_residual_uncertainty() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 30_000,
            residual_p95_us: 20_000,
            residual_max_us: 20_000,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 10_000,
            last_sample_age_us: 2_000,
        });

        estimator.push(sample(10_000, 110_040)).unwrap();
        estimator.push(sample(20_000, 119_990)).unwrap();
        estimator.push(sample(30_000, 130_000)).unwrap();
        estimator.push(sample(40_000, 140_070)).unwrap();

        let health = estimator.health(140_070);
        assert!(
            !health.healthy,
            "fitted drift must fail health even when residual thresholds are loose: {health:?}"
        );
        assert!(
            (health.drift_ppm - 1_000.0).abs() <= 1.0,
            "drift_ppm must report fitted slope drift directly: {health:?}"
        );
    }

    #[test]
    fn positive_receive_delay_outlier_does_not_move_lower_envelope_fit() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 3_000,
            residual_p95_us: 100,
            residual_max_us: 250,
            ..RawClockThresholds::for_tests()
        });

        estimator.push(sample(10_000, 110_000)).unwrap();
        estimator.push(sample(11_000, 111_002)).unwrap();
        estimator.push(sample(12_000, 112_001)).unwrap();
        estimator.push(sample(12_500, 115_500)).unwrap();
        estimator.push(sample(13_000, 113_001)).unwrap();

        let mapped = estimator.map_raw_us(12_500).unwrap();
        assert!(
            (mapped as i64 - 112_500).abs() <= 20,
            "positive receive-delay outlier must not pull the fit upward: {mapped}"
        );
    }

    #[test]
    fn receive_mono_age_is_separate_from_fit_anchor() {
        let thresholds = RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 3_000,
            residual_p95_us: 20,
            residual_max_us: 50,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 2_000,
            last_sample_age_us: 2_000,
        };
        let mut estimator = RawClockEstimator::new(thresholds);

        estimator.push_with_receive_mono_us(sample(10_000, 1_010_000), 110_000).unwrap();
        estimator.push_with_receive_mono_us(sample(11_000, 1_011_000), 115_000).unwrap();
        estimator.push_with_receive_mono_us(sample(12_000, 1_012_000), 120_000).unwrap();
        estimator.push_with_receive_mono_us(sample(13_000, 1_013_000), 123_000).unwrap();

        let health = estimator.health(123_100);
        assert!(health.healthy, "{health:?}");
        assert_eq!(health.residual_p95_us, 0);
        assert_eq!(health.last_sample_age_us, 100);
        assert_eq!(estimator.map_raw_us(12_500), Some(1_012_500));
    }

    #[test]
    fn warmup_window_uses_fit_anchor_duration_not_raw_counter_span() {
        let thresholds = RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 10_000,
            residual_p95_us: 20,
            residual_max_us: 50,
            drift_abs_ppm: 1_000.0,
            sample_gap_max_us: 4_000,
            last_sample_age_us: 2_000,
        };
        let mut estimator = RawClockEstimator::new(thresholds);

        estimator.push_with_receive_mono_us(sample(10_000, 1_010_000), 110_000).unwrap();
        estimator.push_with_receive_mono_us(sample(13_332, 1_013_333), 113_333).unwrap();
        estimator.push_with_receive_mono_us(sample(16_664, 1_016_666), 116_666).unwrap();
        estimator.push_with_receive_mono_us(sample(19_996, 1_020_000), 120_000).unwrap();

        let health = estimator.health(120_100);
        assert!(health.healthy, "{health:?}");
        assert_eq!(health.window_duration_us, 10_000);
    }

    #[test]
    fn retention_keeps_enough_history_for_warmup_window_between_samples() {
        let thresholds = RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 10_000,
            residual_p95_us: 20,
            residual_max_us: 50,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 1_000,
            last_sample_age_us: 2_000,
        };
        let mut estimator = RawClockEstimator::new(thresholds);

        for index in 0..=12 {
            let offset_us = index * 900;
            estimator.push(sample(10_000 + offset_us, 110_000 + offset_us)).unwrap();
        }

        let health = estimator.health(120_900);
        assert!(health.healthy, "{health:?}");
        assert!(health.window_duration_us >= 10_000, "{health:?}");
    }

    #[test]
    fn retention_bridges_continuity_boundary_until_runtime_window_matures() {
        let thresholds = RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 10_000,
            residual_p95_us: 20,
            residual_max_us: 50,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 1_000,
            last_sample_age_us: 2_000,
        };
        let mut estimator = RawClockEstimator::new(thresholds);

        for index in 0..=10 {
            let offset_us = index * 1_000;
            estimator.push(sample(10_000 + offset_us, 110_000 + offset_us)).unwrap();
        }
        assert!(estimator.health(120_000).healthy);

        estimator.mark_continuity_boundary();
        for index in 0..=97 {
            let offset_us = index * 100;
            estimator.push(sample(21_400 + offset_us, 121_400 + offset_us)).unwrap();
        }

        let health = estimator.health(131_100);
        assert!(health.healthy, "{health:?}");
        assert!(health.window_duration_us >= 10_000, "{health:?}");
    }
}
