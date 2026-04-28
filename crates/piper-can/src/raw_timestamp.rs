use std::sync::OnceLock;
use std::time::Instant;

/// Socket receive timestamp details preserved separately from control-grade provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RawTimestampInfo {
    pub can_id: u32,
    pub host_rx_mono_us: u64,
    pub system_ts_us: Option<u64>,
    pub hw_trans_us: Option<u64>,
    pub hw_raw_us: Option<u64>,
}

impl RawTimestampInfo {
    pub fn has_hw_raw_without_hw_trans(&self) -> bool {
        self.hw_raw_us.is_some() && self.hw_trans_us.is_none()
    }
}

/// One raw timestamp sample from a named CAN interface.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RawTimestampSample {
    pub iface: String,
    pub info: RawTimestampInfo,
}

impl RawTimestampSample {
    pub fn has_hw_raw_without_hw_trans(&self) -> bool {
        self.info.has_hw_raw_without_hw_trans()
    }
}

/// Process-local monotonic microsecond clock shared by CAN and driver timing code.
pub fn monotonic_micros() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_timestamp_info_reports_hw_raw_without_hw_trans() {
        let info = RawTimestampInfo {
            can_id: 0x251,
            host_rx_mono_us: 123,
            system_ts_us: Some(100),
            hw_trans_us: None,
            hw_raw_us: Some(99),
        };

        assert!(info.has_hw_raw_without_hw_trans());
    }

    #[test]
    fn raw_timestamp_info_prefers_hw_trans_when_present() {
        let info = RawTimestampInfo {
            can_id: 0x251,
            host_rx_mono_us: 123,
            system_ts_us: Some(100),
            hw_trans_us: Some(101),
            hw_raw_us: Some(99),
        };

        assert!(!info.has_hw_raw_without_hw_trans());
    }

    #[test]
    fn raw_timestamp_sample_carries_iface_and_info() {
        let info = RawTimestampInfo {
            can_id: 0x251,
            host_rx_mono_us: 123,
            system_ts_us: Some(100),
            hw_trans_us: None,
            hw_raw_us: Some(99),
        };
        let sample = RawTimestampSample {
            iface: "can0".to_string(),
            info,
        };

        assert_eq!(sample.iface, "can0");
        assert_eq!(sample.info, info);
        assert!(sample.info.has_hw_raw_without_hw_trans());
    }
}
