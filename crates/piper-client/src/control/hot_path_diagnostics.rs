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
    fault_window_suppressed_repeats: u32,
    pending_recovery_count: u32,
    pending_summary_suppressed_warnings: u32,
    pending_summary_started_at: Option<Instant>,
}

impl HotPathDiagnostics {
    pub(crate) fn record_fault(&mut self, now: Instant, interval: Duration) -> FaultLogDecision {
        self.in_fault = true;

        if self
            .last_fault_emitted_at
            .is_none_or(|last| now.saturating_duration_since(last) >= interval)
        {
            let suppressed_repeats = std::mem::take(&mut self.fault_window_suppressed_repeats);
            self.last_fault_emitted_at = Some(now);
            FaultLogDecision::Emit { suppressed_repeats }
        } else {
            self.fault_window_suppressed_repeats =
                self.fault_window_suppressed_repeats.saturating_add(1);
            self.pending_summary_suppressed_warnings =
                self.pending_summary_suppressed_warnings.saturating_add(1);
            FaultLogDecision::Suppress
        }
    }

    pub(crate) fn record_recovery(&mut self, now: Instant) {
        if !self.in_fault {
            return;
        }

        self.in_fault = false;
        self.fault_window_suppressed_repeats = 0;
        if self.pending_recovery_count == 0 {
            self.pending_summary_started_at = Some(now);
        }
        self.pending_recovery_count = self.pending_recovery_count.saturating_add(1);
    }

    pub(crate) fn poll_recovery_summary(
        &mut self,
        now: Instant,
        interval: Duration,
    ) -> Option<RecoverySummary> {
        if self.pending_recovery_count == 0 {
            return None;
        }

        if now.saturating_duration_since(
            self.pending_summary_started_at
                .expect("pending recovery batches must have a start time"),
        ) < interval
        {
            return None;
        }

        self.take_pending_summary()
    }

    pub(crate) fn force_flush_recovery_summary(
        &mut self,
        _now: Instant,
    ) -> Option<RecoverySummary> {
        if self.pending_recovery_count == 0 {
            return None;
        }

        self.take_pending_summary()
    }

    fn take_pending_summary(&mut self) -> Option<RecoverySummary> {
        self.pending_summary_started_at = None;
        Some(RecoverySummary {
            recovery_count: std::mem::take(&mut self.pending_recovery_count),
            suppressed_fault_warnings: std::mem::take(
                &mut self.pending_summary_suppressed_warnings,
            ),
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
        diagnostics.record_recovery(start + Duration::from_millis(5));

        assert_eq!(
            diagnostics.record_fault(start + Duration::from_millis(5), interval),
            FaultLogDecision::Suppress
        );
    }

    #[test]
    fn recovery_is_only_emitted_after_its_window_matures() {
        let mut diagnostics = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        let _ = diagnostics.record_fault(start, interval);
        let _ = diagnostics.record_fault(start + Duration::from_millis(5), interval);
        let recovered_at = start + Duration::from_millis(5);
        diagnostics.record_recovery(recovered_at);

        assert_eq!(
            diagnostics.poll_recovery_summary(start + Duration::from_millis(10), interval),
            None
        );
        assert_eq!(
            diagnostics.poll_recovery_summary(recovered_at + interval, interval),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 1,
            })
        );
        assert_eq!(
            diagnostics.poll_recovery_summary(recovered_at + interval, interval),
            None
        );
    }

    #[test]
    fn non_forced_flush_respects_summary_interval_and_force_overrides_it() {
        let mut diagnostics = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        let _ = diagnostics.record_fault(start, interval);
        let recovered_at = start;
        diagnostics.record_recovery(recovered_at);
        assert_eq!(diagnostics.poll_recovery_summary(start, interval), None);
        assert_eq!(
            diagnostics.poll_recovery_summary(recovered_at + interval, interval),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );

        let _ = diagnostics.record_fault(start + Duration::from_millis(10), interval);
        diagnostics.record_recovery(start + Duration::from_millis(10));
        assert_eq!(
            diagnostics.poll_recovery_summary(start + Duration::from_millis(20), interval),
            None
        );
        assert_eq!(
            diagnostics.force_flush_recovery_summary(start + Duration::from_millis(20)),
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
        let recovered_at = start;
        send_failures.record_recovery(recovered_at);

        assert_eq!(
            send_failures.poll_recovery_summary(recovered_at + interval, interval),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );
        assert_eq!(
            overruns.poll_recovery_summary(start + interval, interval),
            None
        );
    }

    #[test]
    fn long_gap_fault_warning_does_not_reuse_previous_incident_suppressed_count() {
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
        diagnostics.record_recovery(start + Duration::from_millis(5));

        assert_eq!(
            diagnostics.record_fault(start + interval + Duration::from_millis(5), interval),
            FaultLogDecision::Emit {
                suppressed_repeats: 0
            },
            "a new fault burst must not inherit suppressed warnings from a recovered incident",
        );
    }

    #[test]
    fn pending_recoveries_are_emitted_from_window_poll_before_force_flush() {
        let mut diagnostics = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        let _ = diagnostics.record_fault(start, interval);
        let _ = diagnostics.record_fault(start + Duration::from_millis(5), interval);
        let recovered_at = start + Duration::from_millis(5);
        diagnostics.record_recovery(recovered_at);

        assert_eq!(
            diagnostics.poll_recovery_summary(start + Duration::from_millis(10), interval),
            None,
            "the first runtime recovery summary must wait for a full diagnostics window",
        );
        assert_eq!(
            diagnostics.poll_recovery_summary(recovered_at + interval, interval),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 1,
            }),
            "recovery visibility must come from runtime window polling, not only exit-time flush",
        );
        assert_eq!(
            diagnostics.force_flush_recovery_summary(start + Duration::from_millis(20)),
            None,
            "window poll must drain the pending recovery summary before forced flush",
        );
    }

    #[test]
    fn force_flush_does_not_make_the_next_recovery_batch_immediately_visible() {
        let mut diagnostics = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        let _ = diagnostics.record_fault(start, interval);
        diagnostics.record_recovery(start + Duration::from_millis(20));
        assert_eq!(
            diagnostics.force_flush_recovery_summary(start + Duration::from_millis(10)),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );

        let next_fault_at = start + Duration::from_millis(20);
        let _ = diagnostics.record_fault(next_fault_at, interval);
        let next_recovery = next_fault_at + Duration::from_millis(5);
        diagnostics.record_recovery(next_recovery);
        assert_eq!(
            diagnostics.poll_recovery_summary(next_recovery + Duration::from_millis(10), interval),
            None,
            "forced flush must not let the next recovery batch bypass the diagnostics window",
        );
        assert_eq!(
            diagnostics.poll_recovery_summary(next_recovery + interval, interval),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 1,
            })
        );
    }
}
