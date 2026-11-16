//! Wall-clock time for sources, injectable so tests use explicit times.
//!
//! Schedules and calendars fire at wall-clock instants. A monotonic timer such as tokio's does
//! not advance while the machine is suspended, so a sleep that spans a suspend would fire late by
//! the length of the suspend. On Linux, [`SystemClock::sleep_until`] arms a `CLOCK_REALTIME`
//! timerfd with an absolute deadline instead: it fires on resume if the deadline passed while
//! asleep, and wakes early when the clock is set (`TFD_TIMER_CANCEL_ON_SET`). Elsewhere it sleeps
//! in steps of at most [`FALLBACK_STEP`] and re-reads the clock, which bounds the lateness after
//! a resume. Either way a wake-up may be early: callers re-check [`Clock::now`] and sleep again.

use std::fmt;

use pulse_contracts::Timestamp;

use crate::BoxFuture;

pub trait Clock: Send + Sync + fmt::Debug + 'static {
    fn now(&self) -> Timestamp;

    /// Resolves at or after `at`, or earlier (callers must re-check `now()`).
    fn sleep_until(&self, at: Timestamp) -> BoxFuture<'static, ()>;
}

/// The longest single sleep where no wall-clock timer is available.
pub const FALLBACK_STEP: std::time::Duration = std::time::Duration::from_secs(60);

/// The real clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_system_time(std::time::SystemTime::now())
    }

    fn sleep_until(&self, at: Timestamp) -> BoxFuture<'static, ()> {
        Box::pin(wall_sleep_until(at))
    }
}

async fn stepped_sleep_until(at: Timestamp) {
    let now = SystemClock.now();
    let wait = at.since(now).to_std().min(FALLBACK_STEP);
    tokio::time::sleep(wait).await;
}

#[cfg(target_os = "linux")]
async fn wall_sleep_until(at: Timestamp) {
    match linux::realtime_timer(at) {
        Ok(fd) => {
            // Readable means expired, or cancelled because the clock was set. Both are a reason
            // to re-read the clock, which the caller does.
            let _ = fd.readable().await;
        }
        Err(_) => stepped_sleep_until(at).await,
    }
}

#[cfg(not(target_os = "linux"))]
async fn wall_sleep_until(at: Timestamp) {
    stepped_sleep_until(at).await;
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::fd::OwnedFd;

    use pulse_contracts::Timestamp;
    use rustix::time::{
        Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec, timerfd_create,
        timerfd_settime,
    };
    use tokio::io::unix::AsyncFd;

    pub(super) fn realtime_timer(at: Timestamp) -> std::io::Result<AsyncFd<OwnedFd>> {
        let fd = timerfd_create(
            TimerfdClockId::Realtime,
            TimerfdFlags::NONBLOCK | TimerfdFlags::CLOEXEC,
        )?;
        // An all-zero value disarms the timer, so the earliest deadline is 1 ms after the epoch.
        let ms = at.as_millis().max(1);
        let spec = Itimerspec {
            it_interval: Timespec { tv_sec: 0, tv_nsec: 0 },
            it_value: Timespec { tv_sec: ms / 1000, tv_nsec: (ms % 1000) * 1_000_000 },
        };
        timerfd_settime(&fd, TimerfdTimerFlags::ABSTIME | TimerfdTimerFlags::CANCEL_ON_SET, &spec)?;
        // `AsyncFd::new` is deprecated because it cannot prove the descriptor stays open while
        // registered; here the `AsyncFd` owns it, so it is closed only when deregistered. The
        // replacement, `register`, is `unsafe`, which this workspace forbids.
        #[allow(deprecated)]
        AsyncFd::new(fd)
    }
}

/// A clock on tokio's time: `now` is `base` plus the tokio time elapsed since the clock was
/// made. Under `tokio::time::pause()` time only moves when a test advances it or every task is
/// waiting on a timer, so tests see exact, repeatable times.
#[derive(Clone, Debug)]
pub struct TokioClock {
    base: Timestamp,
    start: tokio::time::Instant,
}

impl TokioClock {
    pub fn new(base: Timestamp) -> Self {
        TokioClock { base, start: tokio::time::Instant::now() }
    }

    fn instant(&self, at: Timestamp) -> tokio::time::Instant {
        self.start + at.since(self.base).to_std()
    }
}

impl Clock for TokioClock {
    fn now(&self) -> Timestamp {
        let elapsed = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.base.saturating_add(pulse_contracts::Duration::from_millis(elapsed))
    }

    fn sleep_until(&self, at: Timestamp) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep_until(self.instant(at)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn system_clock_wakes_for_a_past_deadline() {
        // A deadline in the past fires at once, through the timerfd on Linux.
        SystemClock.sleep_until(Timestamp::from_millis(1)).await;
        let soon = SystemClock.now();
        SystemClock.sleep_until(soon).await;
    }

    #[tokio::test(start_paused = true)]
    async fn tokio_clock_follows_paused_time() {
        let clock = TokioClock::new(Timestamp::from_millis(1_000));
        assert_eq!(clock.now(), Timestamp::from_millis(1_000));
        clock.sleep_until(Timestamp::from_millis(61_000)).await;
        assert_eq!(clock.now(), Timestamp::from_millis(61_000));
    }
}
