//! `schedule.fired` on a cron expression (in the local or a named time zone) or a fixed interval.
//!
//! One wall-clock timer is armed for the next fire; nothing ticks in between (see
//! [`crate::clock`] for why the timer survives suspend).
//!
//! **Missed fires.** Fires that should have happened while the machine slept or the daemon was
//! down produce at most one catch-up fire per schedule, for the latest missed time, with
//! `late: true`; never a backlog. The last fired time is kept in a small state file in the Pulse
//! home, so a restart knows what it missed (and does not repeat a fire). A schedule seen for the
//! first time (or whose expression changed) starts fresh: no catch-up. A fire that comes more
//! than [`LATE_AFTER`] after its time is also marked late.
//!
//! `every` schedules keep their phase: they fire at the start time plus whole intervals, and
//! after a restart continue from the last fire.

mod cron;

use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use pulse_contracts::catalog::{BuiltinEvent, ScheduleFiredData};
use pulse_contracts::config::ScheduleSourceSettings;
use pulse_contracts::{Duration, SourceKind, SourceRef, Timestamp};
use serde::{Deserialize, Serialize};

use self::cron::Cron;
use crate::clock::{Clock, SystemClock};
use crate::state::StateFile;
use crate::{BoxFuture, Source, SourceContext, SourceEnv, SourceError};

/// A fire this long after its scheduled time is reported as late.
pub const LATE_AFTER: Duration = Duration::from_secs(60);
/// The longest `every`: ten years. More is a typo (and near `i64::MAX` milliseconds, arithmetic
/// on it overflows).
const MAX_EVERY: Duration = Duration::from_days(3650);

#[derive(Debug)]
pub struct ScheduleSource {
    source: SourceRef,
    plan: Plan,
    settings: ScheduleSourceSettings,
    state: StateFile,
    clock: Arc<dyn Clock>,
}

#[derive(Clone, Debug)]
enum Zone {
    Local,
    Named(chrono_tz::Tz),
}

#[derive(Clone, Debug)]
enum Plan {
    Cron { cron: Cron, zone: Zone },
    Every(Duration),
}

/// What survives a restart. `key` identifies the schedule's definition; state for a different
/// definition is ignored.
#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    key: String,
    last: Timestamp,
}

fn to_chrono(t: Timestamp) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(t.as_millis())
}

fn from_chrono(t: DateTime<Utc>) -> Timestamp {
    Timestamp::from_millis(t.timestamp_millis())
}

impl Plan {
    /// The first fire strictly after `t`. `every` fires at `anchor` plus whole intervals.
    fn next_after(&self, anchor: Timestamp, t: Timestamp) -> Option<Timestamp> {
        match self {
            Plan::Every(every) => {
                let e = i64::try_from(every.as_millis()).ok()?.max(1);
                let k =
                    t.as_millis().checked_sub(anchor.as_millis())?.div_euclid(e).checked_add(1)?;
                let at = anchor.as_millis().checked_add(k.max(1).checked_mul(e)?)?;
                Some(Timestamp::from_millis(at))
            }
            Plan::Cron { cron, zone } => {
                let after = to_chrono(t)?;
                let next = match zone {
                    Zone::Local => cron.next_after(&chrono::Local, after),
                    Zone::Named(tz) => cron.next_after(tz, after),
                };
                next.map(from_chrono)
            }
        }
    }

    /// The latest fire time in `(last, now]`, if any (for `every`, `last` is the anchor).
    fn latest_due(&self, last: Timestamp, now: Timestamp) -> Option<Timestamp> {
        let first = self.next_after(last, last)?;
        if first > now {
            return None;
        }
        if let Plan::Every(every) = self {
            let e = i64::try_from(every.as_millis()).ok()?.max(1);
            let k = now.as_millis().checked_sub(last.as_millis())?.div_euclid(e);
            return Some(Timestamp::from_millis(last.as_millis().checked_add(k.checked_mul(e)?)?));
        }
        // Walk forward; a schedule missed for months at one fire a minute is the worst case, so
        // after a bound, restart the walk close to `now`.
        let mut due = first;
        for _ in 0..100_000 {
            match self.next_after(last, due) {
                Some(n) if n <= now => due = n,
                _ => return Some(due),
            }
        }
        let mut near = self.next_after(last, now.saturating_sub(Duration::from_days(2)));
        while let Some(n) = near.filter(|n| *n <= now) {
            due = n;
            near = self.next_after(last, n);
        }
        Some(due)
    }
}

impl ScheduleSource {
    pub fn new(
        name: &str,
        settings: &ScheduleSourceSettings,
        env: &SourceEnv,
    ) -> Result<Self, SourceError> {
        let plan = match (&settings.cron, settings.every) {
            (Some(expr), None) => {
                let cron = Cron::parse(expr).map_err(SourceError::Config)?;
                let zone = match &settings.timezone {
                    None => Zone::Local,
                    Some(name) => Zone::Named(chrono_tz::Tz::from_str(name).map_err(|_| {
                        SourceError::Config(format!(
                            "unknown time zone {name:?}; use an IANA name like \"Europe/Rome\""
                        ))
                    })?),
                };
                let plan = Plan::Cron { cron, zone };
                if plan.next_after(Timestamp::EPOCH, SystemClock.now()).is_none() {
                    return Err(SourceError::Config(format!("cron {expr:?} never fires")));
                }
                plan
            }
            (None, Some(every)) if every > MAX_EVERY => {
                return Err(SourceError::Config(format!("every must be at most {MAX_EVERY}")));
            }
            (None, Some(every)) if !every.is_zero() => Plan::Every(every),
            _ => {
                return Err(SourceError::Config(
                    "set exactly one of `cron` and `every` (positive)".into(),
                ));
            }
        };
        let source = SourceRef::new(SourceKind::Schedule, name);
        Ok(ScheduleSource {
            state: StateFile::new(env, &source),
            source,
            plan,
            settings: settings.clone(),
            clock: Arc::new(SystemClock),
        })
    }

    fn key(&self) -> String {
        let s = &self.settings;
        format!("{:?}|{:?}|{:?}", s.cron, s.every.map(|d| d.to_string()), s.timezone)
    }

    async fn fire(
        &self,
        cx: &SourceContext,
        due: Timestamp,
        now: Timestamp,
    ) -> Result<(), SourceError> {
        let ev = BuiltinEvent::ScheduleFired(ScheduleFiredData {
            cron: self.settings.cron.clone(),
            every: self.settings.every,
            scheduled_for: due,
            late: now.since(due) > LATE_AFTER,
        })
        .into_new_event(self.source.clone(), now);
        tokio::select! {
            () = cx.shutdown.cancelled() => Ok(()),
            r = cx.events.send(ev) => r,
        }?;
        self.save(cx, due);
        Ok(())
    }

    fn save(&self, cx: &SourceContext, last: Timestamp) {
        if let Err(e) = self.state.store(&Saved { key: self.key(), last }) {
            cx.health.error(format!("cannot save schedule state: {e}"));
        }
    }

    async fn run(self, cx: &SourceContext) -> Result<(), SourceError> {
        let now = self.clock.now();
        let saved = self.state.load::<Saved>().filter(|s| s.key == self.key());
        let mut last = match saved {
            Some(s) => match self.plan.latest_due(s.last, now) {
                Some(due) => {
                    self.fire(cx, due, now).await?;
                    due
                }
                None => s.last,
            },
            None => {
                self.save(cx, now);
                now
            }
        };
        cx.health.healthy();
        loop {
            let now = self.clock.now();
            let Some(next) = self.plan.next_after(last, last.max(now)) else {
                cx.health.degraded("this schedule has no future fire times");
                cx.shutdown.cancelled().await;
                return Ok(());
            };
            tokio::select! {
                () = cx.shutdown.cancelled() => return Ok(()),
                () = self.clock.sleep_until(next) => {}
            }
            let now = self.clock.now();
            if now < next {
                continue;
            }
            // Woken late (suspend, clock change): one fire for the latest due time.
            let due = self.plan.latest_due(last, now).unwrap_or(next);
            self.fire(cx, due, now).await?;
            last = due;
        }
    }
}

impl Source for ScheduleSource {
    fn source_ref(&self) -> &SourceRef {
        &self.source
    }

    fn run(self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            let result = (*self).run(&cx).await;
            match &result {
                Ok(()) => cx.health.stopped(),
                Err(e) => cx.health.failed(e.to_string()),
            }
            result
        })
    }
}

#[cfg(test)]
mod tests;
