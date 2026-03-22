use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SleepStrategy {
    Sleep,
    Spin,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CycleTick {
    pub tick_start: Instant,
    pub real_dt: Duration,
    pub lag: Duration,
    pub missed_deadlines: u64,
}

#[derive(Debug)]
pub(crate) struct CycleScheduler {
    period: Duration,
    strategy: SleepStrategy,
    next_deadline: Instant,
    last_tick_start: Instant,
}

impl CycleScheduler {
    pub(crate) fn new(period: Duration, strategy: SleepStrategy) -> Self {
        let now = Instant::now();
        Self {
            period,
            strategy,
            // Delay the first tick by one nominal period so the first reported dt
            // is close to the configured cycle time instead of near zero.
            next_deadline: now + period,
            last_tick_start: now,
        }
    }

    pub(crate) fn wait_next(&mut self) -> CycleTick {
        let deadline = self.next_deadline;
        sleep_until(self.strategy, deadline);

        let tick_start = Instant::now();
        let real_dt = tick_start.saturating_duration_since(self.last_tick_start);
        let lag = tick_start.saturating_duration_since(deadline);

        let mut next_deadline = deadline + self.period;
        let mut missed_deadlines = 0u64;
        while next_deadline <= tick_start {
            next_deadline += self.period;
            missed_deadlines += 1;
        }

        self.last_tick_start = tick_start;
        self.next_deadline = next_deadline;

        CycleTick {
            tick_start,
            real_dt,
            lag,
            missed_deadlines,
        }
    }
}

fn sleep_until(strategy: SleepStrategy, deadline: Instant) {
    let now = Instant::now();
    if deadline <= now {
        return;
    }

    let sleep_duration = deadline - now;
    match strategy {
        SleepStrategy::Sleep => std::thread::sleep(sleep_duration),
        SleepStrategy::Spin => spin_sleep::SpinSleeper::default().sleep(sleep_duration),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cycle_scheduler_first_tick_waits_for_nominal_period() {
        let period = Duration::from_millis(20);
        let mut scheduler = CycleScheduler::new(period, SleepStrategy::Sleep);

        let first = scheduler.wait_next();
        assert!(first.real_dt >= Duration::from_millis(15));
        assert_eq!(first.missed_deadlines, 0);
        assert!(first.lag < Duration::from_millis(20));
    }

    #[test]
    fn test_cycle_scheduler_reports_nominal_second_tick() {
        let period = Duration::from_millis(20);
        let mut scheduler = CycleScheduler::new(period, SleepStrategy::Sleep);

        let _first = scheduler.wait_next();

        let second = scheduler.wait_next();
        assert!(second.real_dt >= Duration::from_millis(15));
        assert!(second.missed_deadlines <= 1);
        assert!(second.lag < Duration::from_millis(20));
    }

    #[test]
    fn test_cycle_scheduler_catches_up_after_overrun() {
        let period = Duration::from_millis(10);
        let mut scheduler = CycleScheduler::new(period, SleepStrategy::Sleep);

        scheduler.next_deadline = Instant::now() - Duration::from_millis(25);
        scheduler.last_tick_start = Instant::now() - Duration::from_millis(35);

        let tick = scheduler.wait_next();
        assert!(tick.real_dt >= Duration::from_millis(30));
        assert!(tick.lag >= Duration::from_millis(20));
        assert!(tick.missed_deadlines >= 1);
        assert!(scheduler.next_deadline > tick.tick_start);
    }
}
