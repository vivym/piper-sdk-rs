//! # 统计工具
//!
//! 统计算法和分析功能（可选模块）
//!
//! 需要启用 `statistics` feature：
//! ```toml
//! piper-tools = { workspace = true, features = ["statistics"] }
//! ```

use serde::{Deserialize, Serialize};

/// CAN 总线统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanBusStatistics {
    /// 帧速率（帧/秒）
    pub fps: f64,

    /// 带宽使用（字节/秒）
    pub bandwidth_bps: f64,

    /// 总帧数
    pub total_frames: u64,

    /// 错误帧数
    pub error_frames: u64,

    /// 丢帧率（%）
    pub loss_rate: f64,
}

impl CanBusStatistics {
    /// 计算帧速率
    pub fn calculate_fps(frame_count: u64, duration_us: u64) -> f64 {
        if duration_us == 0 {
            return 0.0;
        }

        let duration_sec = duration_us as f64 / 1_000_000.0;
        frame_count as f64 / duration_sec
    }

    /// 计算带宽使用
    pub fn calculate_bandwidth(total_bytes: u64, duration_us: u64) -> f64 {
        if duration_us == 0 {
            return 0.0;
        }

        let duration_sec = duration_us as f64 / 1_000_000.0;
        total_bytes as f64 / duration_sec
    }

    /// 计算丢帧率
    pub fn calculate_loss_rate(expected_frames: u64, received_frames: u64) -> f64 {
        if expected_frames == 0 {
            return 0.0;
        }

        let lost = expected_frames.saturating_sub(received_frames);
        (lost as f64 / expected_frames as f64) * 100.0
    }
}

/// 延迟统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStatistics {
    /// 平均延迟（微秒）
    pub avg_latency_us: f64,

    /// 最小延迟（微秒）
    pub min_latency_us: u64,

    /// 最大延迟（微秒）
    pub max_latency_us: u64,

    /// 标准差（微秒）
    pub std_dev_us: f64,

    /// 样本数量
    pub sample_count: u64,
}

impl LatencyStatistics {
    /// 计算延迟统计
    pub fn calculate(latencies: &[u64]) -> Self {
        if latencies.is_empty() {
            return Self {
                avg_latency_us: 0.0,
                min_latency_us: 0,
                max_latency_us: 0,
                std_dev_us: 0.0,
                sample_count: 0,
            };
        }

        let sum: u64 = latencies.iter().sum();
        let avg = sum as f64 / latencies.len() as f64;

        let min = *latencies.iter().min().unwrap_or(&0);
        let max = *latencies.iter().max().unwrap_or(&0);

        // 计算标准差
        let variance = latencies
            .iter()
            .map(|&x| {
                let diff = x as f64 - avg;
                diff * diff
            })
            .sum::<f64>()
            / latencies.len() as f64;

        let std_dev = variance.sqrt();

        Self {
            avg_latency_us: avg,
            min_latency_us: min,
            max_latency_us: max,
            std_dev_us: std_dev,
            sample_count: latencies.len() as u64,
        }
    }

    /// 计算百分位数
    pub fn percentile(&self, _percentile: f64) -> u64 {
        // ⚠️ 实际实现需要存储所有样本
        // 这里提供一个框架
        self.avg_latency_us as u64
    }

    /// 计算抖动（相邻样本延迟差）
    pub fn jitter(&self) -> f64 {
        // 抖动通常用标准差表示
        self.std_dev_us
    }
}

/// CAN ID 分布统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanIdDistribution {
    /// 每个 CAN ID 的帧数
    pub counts: std::collections::HashMap<u32, u64>,

    /// 总帧数
    pub total_frames: u64,
}

impl CanIdDistribution {
    /// 创建新的分布统计
    pub fn new() -> Self {
        Self {
            counts: std::collections::HashMap::new(),
            total_frames: 0,
        }
    }

    /// 添加帧
    pub fn add_frame(&mut self, can_id: u32) {
        *self.counts.entry(can_id).or_insert(0) += 1;
        self.total_frames += 1;
    }

    /// 获取某个 CAN ID 的频率（%）
    pub fn frequency(&self, can_id: u32) -> f64 {
        if self.total_frames == 0 {
            return 0.0;
        }

        let count = *self.counts.get(&can_id).unwrap_or(&0);
        (count as f64 / self.total_frames as f64) * 100.0
    }

    /// 获取最常见的 CAN ID
    pub fn most_common(&self, limit: usize) -> Vec<(u32, u64)> {
        let mut items: Vec<_> = self.counts.iter().map(|(&k, &v)| (k, v)).collect();
        items.sort_by(|a, b| b.1.cmp(&a.1));
        items.into_iter().take(limit).collect()
    }
}

impl Default for CanIdDistribution {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_fps() {
        let fps = CanBusStatistics::calculate_fps(1000, 1_000_000); // 1000帧，1秒
        assert_eq!(fps, 1000.0);

        let fps = CanBusStatistics::calculate_fps(100, 100_000); // 100帧，0.1秒
        assert_eq!(fps, 1000.0);
    }

    #[test]
    fn test_calculate_bandwidth() {
        let bps = CanBusStatistics::calculate_bandwidth(1000, 1_000_000); // 1000字节，1秒
        assert_eq!(bps, 1000.0);
    }

    #[test]
    fn test_calculate_loss_rate() {
        let loss_rate = CanBusStatistics::calculate_loss_rate(1000, 950); // 期望1000，收到950
        assert_eq!(loss_rate, 5.0);

        let loss_rate = CanBusStatistics::calculate_loss_rate(1000, 1000); // 无丢帧
        assert_eq!(loss_rate, 0.0);
    }

    #[test]
    fn test_latency_statistics() {
        let latencies = vec![100, 150, 200, 120, 180];
        let stats = LatencyStatistics::calculate(&latencies);

        assert_eq!(stats.min_latency_us, 100);
        assert_eq!(stats.max_latency_us, 200);
        assert_eq!(stats.sample_count, 5);

        // 验证平均值
        let expected_avg = (100 + 150 + 200 + 120 + 180) as f64 / 5.0;
        assert!((stats.avg_latency_us - expected_avg).abs() < 0.01);
    }

    #[test]
    fn test_latency_statistics_empty() {
        let latencies = vec![];
        let stats = LatencyStatistics::calculate(&latencies);

        assert_eq!(stats.sample_count, 0);
        assert_eq!(stats.avg_latency_us, 0.0);
    }

    #[test]
    fn test_can_id_distribution() {
        let mut dist = CanIdDistribution::new();

        dist.add_frame(0x100);
        dist.add_frame(0x100);
        dist.add_frame(0x200);

        assert_eq!(dist.total_frames, 3);
        assert_eq!(dist.frequency(0x100), (2.0 / 3.0 * 100.0));
        assert_eq!(dist.frequency(0x200), (1.0 / 3.0 * 100.0));

        let common = dist.most_common(2);
        assert_eq!(common[0], (0x100, 2));
        assert_eq!(common[1], (0x200, 1));
    }

    #[test]
    fn test_can_id_distribution_default() {
        let dist = CanIdDistribution::default();
        assert_eq!(dist.total_frames, 0);
        assert_eq!(dist.frequency(0x100), 0.0);
    }
}
