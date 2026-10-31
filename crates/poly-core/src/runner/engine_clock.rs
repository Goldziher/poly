//! Wall and thread-CPU time of one engine call.
//!
//! Wall time alone cannot tell an engine that is slow from an engine that sat
//! behind a lock or a saturated core while other workers ran. The thread's CPU
//! time can: it stops counting whenever the thread is not executing.

use std::time::{Duration, Instant};

/// A running measurement of one engine call.
pub(super) struct EngineClock {
    wall: Instant,
    cpu: Option<Duration>,
}

/// Elapsed wall time and, where the platform reports it, the CPU time the
/// calling thread spent in the same interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EngineTime {
    pub(super) wall: Duration,
    pub(super) cpu: Option<Duration>,
}

impl EngineTime {
    /// Whether the call was mostly waiting rather than computing: its CPU time
    /// is under half its wall time. `false` when CPU time is unavailable, so an
    /// unmeasurable call is never excused.
    pub(super) fn mostly_waiting(&self) -> bool {
        self.cpu.is_some_and(|cpu| cpu * 2 < self.wall)
    }
}

impl EngineClock {
    pub(super) fn start() -> Self {
        Self {
            wall: Instant::now(),
            cpu: thread_cpu_time(),
        }
    }

    pub(super) fn started(&self) -> Instant {
        self.wall
    }

    pub(super) fn stop(&self) -> EngineTime {
        EngineTime {
            wall: self.wall.elapsed(),
            cpu: self
                .cpu
                .zip(thread_cpu_time())
                .map(|(start, end)| end.saturating_sub(start)),
        }
    }
}

#[cfg(unix)]
fn thread_cpu_time() -> Option<Duration> {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    (status == 0).then(|| Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
}

#[cfg(not(unix))]
fn thread_cpu_time() -> Option<Duration> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeping_is_waiting_not_working() {
        let clock = EngineClock::start();
        std::thread::sleep(Duration::from_millis(80));
        let time = clock.stop();
        assert!(time.wall >= Duration::from_millis(80));
        if time.cpu.is_some() {
            assert!(time.mostly_waiting(), "{time:?}");
        }
    }

    #[test]
    fn spinning_is_working() {
        // Spin until the thread itself has run for 40ms rather than for a wall-clock interval: on a
        // loaded CI runner the scheduler can preempt a wall-clock spin for most of its window. ~keep
        let target = Duration::from_millis(40);
        let clock = EngineClock::start();
        let Some(start) = clock.cpu else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while thread_cpu_time().is_some_and(|now| now.saturating_sub(start) < target) && Instant::now() < deadline {
            std::hint::spin_loop();
        }
        let time = clock.stop();
        assert!(time.cpu.is_some_and(|cpu| cpu >= target), "{time:?}");
    }

    #[test]
    fn unmeasurable_cpu_is_never_excused() {
        let time = EngineTime {
            wall: Duration::from_secs(5),
            cpu: None,
        };
        assert!(!time.mostly_waiting());
    }
}
