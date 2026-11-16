//! Time for the daemon, and the ids it assigns.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use pulse_contracts::{DeliveryId, EventId, RunId, SubscriptionId, Timestamp, Ulid};

/// Where "now" comes from. Timers are armed for timestamps, so the clock and the timers must
/// agree.
#[derive(Clone, Copy, Debug)]
pub enum Clock {
    /// The wall clock. Timers are armed for `deadline - now`, so a clock change while a timer
    /// is pending moves the deadline by that much (acceptable for debounces and retries).
    System,
    /// Follows tokio's clock from a fixed start, so tests with `tokio::time::pause()` control
    /// time completely.
    Tokio { start_ms: i64, start: tokio::time::Instant },
}

impl Clock {
    /// A tokio-driven clock starting at `start_ms`. Call inside the runtime.
    pub fn tokio(start_ms: i64) -> Clock {
        Clock::Tokio { start_ms, start: tokio::time::Instant::now() }
    }

    pub fn now(&self) -> Timestamp {
        match self {
            Clock::System => Timestamp::from_system_time(SystemTime::now()),
            Clock::Tokio { start_ms, start } => {
                let elapsed = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
                Timestamp::from_millis(start_ms.saturating_add(elapsed))
            }
        }
    }

    /// The tokio instant at which this clock reads `at`.
    pub fn instant_at(&self, at: Timestamp) -> tokio::time::Instant {
        tokio::time::Instant::now() + at.since(self.now()).to_std()
    }
}

/// Monotonic ULIDs: ids assigned in this process sort in assignment order, even within one
/// millisecond.
#[derive(Debug)]
pub struct Ids {
    clock: Clock,
    generator: Mutex<ulid::Generator>,
}

impl Ids {
    pub fn new(clock: Clock) -> Self {
        Ids { clock, generator: Mutex::new(ulid::Generator::new()) }
    }

    fn next(&self) -> Ulid {
        let ms = u64::try_from(self.clock.now().as_millis()).unwrap_or(0);
        let at = UNIX_EPOCH + std::time::Duration::from_millis(ms);
        let mut g = self.generator.lock().unwrap_or_else(|p| p.into_inner());
        // Overflow needs 2^80 ids in one millisecond; fall back to a random id rather than fail.
        g.generate_from_datetime(at).unwrap_or_else(|_| Ulid::from_datetime(at))
    }

    pub fn event(&self) -> EventId {
        EventId::from_ulid(self.next())
    }
    pub fn run(&self) -> RunId {
        RunId::from_ulid(self.next())
    }
    pub fn delivery(&self) -> DeliveryId {
        DeliveryId::from_ulid(self.next())
    }
    pub fn subscription(&self) -> SubscriptionId {
        SubscriptionId::from_ulid(self.next())
    }
}
