//! Fire history and the limits computed from it: cooldown, rate limit, circuit breaker.
//!
//! One bounded log of fire times per rule serves all three. "Fire" means a decision that
//! created a run (`fire` or `await_approval`); blocked and debounced decisions do not count.

use std::collections::VecDeque;

use pulse_contracts::config::{CircuitBreaker, Rate};
use pulse_contracts::{Duration, Timestamp};

/// The most recent fire times, newest last, bounded by `capacity`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FireHistory {
    times: VecDeque<Timestamp>,
}

impl FireHistory {
    pub fn from_times(mut times: Vec<Timestamp>, capacity: usize) -> Self {
        times.sort();
        let skip = times.len().saturating_sub(capacity);
        FireHistory { times: times.into_iter().skip(skip).collect() }
    }

    pub fn record(&mut self, at: Timestamp, capacity: usize) {
        self.times.push_back(at);
        while self.times.len() > capacity.max(1) {
            self.times.pop_front();
        }
    }

    pub fn last(&self) -> Option<Timestamp> {
        self.times.back().copied()
    }

    /// Fires strictly after `since`.
    pub fn count_after(&self, since: Timestamp) -> usize {
        self.times.iter().rev().take_while(|&&t| t > since).count()
    }

    /// The `n`th most recent fire (0 = newest).
    pub fn nth_newest(&self, n: usize) -> Option<Timestamp> {
        self.times.iter().rev().nth(n).copied()
    }

    pub fn times(&self) -> impl Iterator<Item = Timestamp> + '_ {
        self.times.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.times.len()
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }
}

/// How much history a rule needs for its limits.
pub fn history_capacity(rate: Option<Rate>, breaker: &CircuitBreaker) -> usize {
    let rate = rate.map_or(0, |r| r.count as usize);
    rate.max(breaker.max_fires as usize).max(1)
}

/// `Some(until)` if a fire now is inside the cooldown after the last fire.
pub fn cooldown_until(
    cooldown: Duration,
    history: &FireHistory,
    now: Timestamp,
) -> Option<Timestamp> {
    let until = history.last()?.saturating_add(cooldown);
    (until > now).then_some(until)
}

/// Sliding window: at most `rate.count` fires in any window of `rate.per`. `Some(until)` when
/// full, where `until` is when the oldest fire in the window leaves it.
pub fn rate_limited_until(rate: Rate, history: &FireHistory, now: Timestamp) -> Option<Timestamp> {
    let window_start = now.saturating_sub(rate.per);
    if history.count_after(window_start) < rate.count as usize {
        return None;
    }
    let oldest_counted = history.nth_newest(rate.count as usize - 1)?;
    Some(oldest_counted.saturating_add(rate.per))
}

/// Whether one more fire now would exceed the breaker's `max_fires` within its window.
pub fn breaker_trips(breaker: &CircuitBreaker, history: &FireHistory, now: Timestamp) -> bool {
    history.count_after(now.saturating_sub(breaker.window)) >= breaker.max_fires as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(ms: &[i64]) -> FireHistory {
        FireHistory::from_times(ms.iter().map(|&m| Timestamp::from_millis(m)).collect(), 100)
    }

    #[test]
    fn cooldown() {
        let cd = Duration::from_secs(30);
        let t = Timestamp::from_millis;
        let cases = [
            (&[][..], 0, None),
            (&[0][..], 29_999, Some(30_000)),
            (&[0][..], 30_000, None),
            (&[0, 10_000][..], 30_000, Some(40_000)),
        ];
        for (h, now, want) in cases {
            assert_eq!(cooldown_until(cd, &hist(h), t(now)), want.map(t), "{h:?} at {now}");
        }
    }

    #[test]
    fn rate_limit_is_a_sliding_window() {
        let rate: Rate = "3/1m".parse().unwrap();
        let t = Timestamp::from_millis;
        let cases = [
            (&[][..], 0, None),
            (&[0, 1_000][..], 2_000, None),
            (&[0, 1_000, 2_000][..], 3_000, Some(60_000)),
            (&[0, 1_000, 2_000][..], 59_999, Some(60_000)),
            (&[0, 1_000, 2_000][..], 60_000, None),
            (&[0, 1_000, 2_000, 60_000][..], 60_500, Some(61_000)),
            (&[0, 30_000, 50_000, 55_000][..], 56_000, Some(90_000)),
        ];
        for (h, now, want) in cases {
            assert_eq!(rate_limited_until(rate, &hist(h), t(now)), want.map(t), "{h:?} at {now}");
        }
    }

    #[test]
    fn breaker_and_capacity() {
        let b = CircuitBreaker {
            max_fires: 3,
            window: Duration::from_mins(1),
            open_for: Duration::from_hours(1),
        };
        let t = Timestamp::from_millis;
        assert!(!breaker_trips(&b, &hist(&[0, 1, 2]), t(60_000)));
        assert!(breaker_trips(&b, &hist(&[1, 2, 3]), t(60_000)));
        assert!(!breaker_trips(&b, &hist(&[1, 2]), t(60_000)));
        let mut h = FireHistory::default();
        for i in 0..10 {
            h.record(t(i), 4);
        }
        assert_eq!(h.times().collect::<Vec<_>>(), [6, 7, 8, 9].map(t));
        assert_eq!(history_capacity(Some("50/h".parse().unwrap()), &b), 50);
        assert_eq!(history_capacity(None, &b), 3);
    }
}
