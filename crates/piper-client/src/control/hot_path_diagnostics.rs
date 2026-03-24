use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultLogDecision {
    Emit { suppressed_repeats: u32 },
    Suppress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecoverySummary {
    pub(crate) recovery_count: u32,
    pub(crate) suppressed_fault_warnings: u32,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct HotPathDiagnostics {
    in_fault: bool,
    last_fault_emitted_at: Option<Instant>,
    suppressed_faults_since_last_warning: u32,
    pending_recovery_count: u32,
    pending_suppressed_fault_warnings: u32,
    last_summary_emitted_at: Option<Instant>,
}

impl HotPathDiagnostics {
    pub(crate) fn record_fault(&mut self, now: Instant, interval: Duration) -> FaultLogDecision {
        self.in_fault = true;

        if self
            .last_fault_emitted_at
            .is_none_or(|last| now.saturating_duration_since(last) >= interval)
        {
            let suppressed_repeats = std::mem::take(&mut self.suppressed_faults_since_last_warning);
            self.last_fault_emitted_at = Some(now);
            FaultLogDecision::Emit { suppressed_repeats }
        } else {
            self.suppressed_faults_since_last_warning =
                self.suppressed_faults_since_last_warning.saturating_add(1);
            self.pending_suppressed_fault_warnings =
                self.pending_suppressed_fault_warnings.saturating_add(1);
            FaultLogDecision::Suppress
        }
    }

    pub(crate) fn record_recovery(&mut self) {
        if !self.in_fault {
            return;
        }

        self.in_fault = false;
        self.pending_recovery_count = self.pending_recovery_count.saturating_add(1);
    }

    pub(crate) fn flush_recovery_summary(
        &mut self,
        now: Instant,
        interval: Duration,
        force: bool,
    ) -> Option<RecoverySummary> {
        if self.pending_recovery_count == 0 {
            return None;
        }

        if !force
            && self
                .last_summary_emitted_at
                .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return None;
        }

        self.last_summary_emitted_at = Some(now);
        Some(RecoverySummary {
            recovery_count: std::mem::take(&mut self.pending_recovery_count),
            suppressed_fault_warnings: std::mem::take(&mut self.pending_suppressed_fault_warnings),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_fault_emits_and_repeated_faults_are_suppressed() {
        let mut diagnostics = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        assert_eq!(
            diagnostics.record_fault(start, interval),
            FaultLogDecision::Emit {
                suppressed_repeats: 0
            }
        );
        assert_eq!(
            diagnostics.record_fault(start + Duration::from_millis(5), interval),
            FaultLogDecision::Suppress
        );
    }

    #[test]
    fn recovery_does_not_rearm_fault_warning_inside_same_window() {
        let mut diagnostics = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        assert_eq!(
            diagnostics.record_fault(start, interval),
            FaultLogDecision::Emit {
                suppressed_repeats: 0
            }
        );
        diagnostics.record_recovery();

        assert_eq!(
            diagnostics.record_fault(start + Duration::from_millis(5), interval),
            FaultLogDecision::Suppress
        );
    }

    #[test]
    fn recovery_is_only_emitted_from_flush_and_flush_is_idempotent() {
        let mut diagnostics = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        let _ = diagnostics.record_fault(start, interval);
        let _ = diagnostics.record_fault(start + Duration::from_millis(5), interval);
        diagnostics.record_recovery();

        assert_eq!(
            diagnostics.flush_recovery_summary(start + Duration::from_millis(10), interval, false),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 1,
            })
        );
        assert_eq!(
            diagnostics.flush_recovery_summary(start + Duration::from_millis(10), interval, false),
            None
        );
    }

    #[test]
    fn non_forced_flush_respects_summary_interval_and_force_overrides_it() {
        let mut diagnostics = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        let _ = diagnostics.record_fault(start, interval);
        diagnostics.record_recovery();
        assert_eq!(
            diagnostics.flush_recovery_summary(start, interval, false),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );

        let _ = diagnostics.record_fault(start + Duration::from_millis(10), interval);
        diagnostics.record_recovery();
        assert_eq!(
            diagnostics.flush_recovery_summary(start + Duration::from_millis(20), interval, false),
            None
        );
        assert_eq!(
            diagnostics.flush_recovery_summary(start + Duration::from_millis(20), interval, true),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 1,
            })
        );
    }

    #[test]
    fn separate_trackers_do_not_share_state() {
        let mut send_failures = HotPathDiagnostics::default();
        let mut overruns = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        let _ = send_failures.record_fault(start, interval);
        send_failures.record_recovery();

        assert_eq!(
            send_failures.flush_recovery_summary(start, interval, false),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );
        assert_eq!(
            overruns.flush_recovery_summary(start, interval, false),
            None
        );
    }
}
