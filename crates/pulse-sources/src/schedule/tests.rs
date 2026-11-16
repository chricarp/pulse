use std::sync::Mutex;

use pulse_contracts::NewEvent;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::clock::TokioClock;

fn ts(s: &str) -> Timestamp {
    Timestamp::parse_rfc3339(s).unwrap()
}

/// A clock on paused tokio time whose wake-ups can come late, like a laptop that slept through
/// its timer.
#[derive(Debug)]
struct SleepyClock {
    inner: TokioClock,
    /// The wake-up for this deadline comes this much later.
    oversleep: Mutex<Option<(Timestamp, Duration)>>,
}

impl Clock for SleepyClock {
    fn now(&self) -> Timestamp {
        self.inner.now()
    }

    fn sleep_until(&self, at: Timestamp) -> BoxFuture<'static, ()> {
        let extra = match *self.oversleep.lock().unwrap() {
            Some((deadline, extra)) if deadline == at => extra,
            _ => Duration::ZERO,
        };
        self.inner.sleep_until(at.saturating_add(extra))
    }
}

fn source(cron: Option<&str>, every: Option<&str>, home: &std::path::Path) -> ScheduleSource {
    let settings = ScheduleSourceSettings {
        cron: cron.map(str::to_owned),
        every: every.map(|e| Duration::parse(e).unwrap()),
        timezone: cron.map(|_| "Europe/Rome".to_owned()),
    };
    let env = SourceEnv { home: home.into(), pulse_home: home.into() };
    ScheduleSource::new("daily", &settings, &env).unwrap()
}

struct Run {
    rx: mpsc::Receiver<NewEvent>,
    stop: CancellationToken,
    clock: Arc<SleepyClock>,
}

fn start(mut src: ScheduleSource, now: &str) -> Run {
    let clock =
        Arc::new(SleepyClock { inner: TokioClock::new(ts(now)), oversleep: Mutex::new(None) });
    src.clock = clock.clone();
    let stop = CancellationToken::new();
    let (cx, rx, _health) = crate::context(src.source.clone(), 8, stop.clone(), Timestamp::EPOCH);
    tokio::spawn(Box::new(src).run(cx));
    Run { rx, stop, clock }
}

impl Run {
    /// The next fire: (scheduled_for, late, emitted at).
    async fn next(&mut self) -> (String, bool, String) {
        let e = self.rx.recv().await.unwrap();
        assert_eq!(e.type_.as_str(), "schedule.fired");
        assert_eq!(e.subject.as_deref(), Some("daily"));
        (
            e.data["scheduled_for"].as_str().unwrap().to_owned(),
            e.data["late"].as_bool().unwrap(),
            e.time.to_string(),
        )
    }
}

fn fire(at: &str, late: bool, emitted: &str) -> (String, bool, String) {
    (at.into(), late, emitted.into())
}

#[tokio::test(start_paused = true)]
async fn cron_fires_on_time_across_dst_and_catches_up_once_after_downtime() {
    let home = tempfile::tempdir().unwrap();
    // First start on a Saturday evening: no catch-up for a schedule never seen before.
    let mut r = start(source(Some("0 9 * * *"), None, home.path()), "2026-03-28T20:00:00Z");
    // 09:00 in Rome is 08:00Z before the spring-forward night and 07:00Z after it.
    assert_eq!(r.next().await, fire("2026-03-29T07:00:00.000Z", false, "2026-03-29T07:00:00.000Z"));
    assert_eq!(r.next().await, fire("2026-03-30T07:00:00.000Z", false, "2026-03-30T07:00:00.000Z"));
    r.stop.cancel();

    // The daemon is down for three days. On restart: exactly one late fire for the latest
    // missed time, then back on schedule.
    let mut r = start(source(Some("0 9 * * *"), None, home.path()), "2026-04-02T13:00:00Z");
    assert_eq!(r.next().await, fire("2026-04-02T07:00:00.000Z", true, "2026-04-02T13:00:00.000Z"));
    assert_eq!(r.next().await, fire("2026-04-03T07:00:00.000Z", false, "2026-04-03T07:00:00.000Z"));
    r.stop.cancel();

    // Restarted before the next fire: nothing is repeated.
    let mut r = start(source(Some("0 9 * * *"), None, home.path()), "2026-04-03T08:00:00Z");
    assert_eq!(r.next().await, fire("2026-04-04T07:00:00.000Z", false, "2026-04-04T07:00:00.000Z"));
    r.stop.cancel();

    // A changed expression is a new schedule: no catch-up from the old one's state.
    let mut r = start(source(Some("30 9 * * *"), None, home.path()), "2026-04-10T12:00:00Z");
    assert_eq!(r.next().await, fire("2026-04-11T07:30:00.000Z", false, "2026-04-11T07:30:00.000Z"));
}

#[tokio::test(start_paused = true)]
async fn sleeping_through_several_fires_produces_one_late_fire() {
    let home = tempfile::tempdir().unwrap();
    let mut r = start(source(Some("0 * * * *"), None, home.path()), "2026-11-16T09:30:00Z");
    // The machine sleeps through 11:00 to 15:00 and wakes at 15:20.
    *r.clock.oversleep.lock().unwrap() =
        Some((ts("2026-11-16T11:00:00Z"), Duration::parse("4h20m").unwrap()));
    assert_eq!(r.next().await, fire("2026-11-16T10:00:00.000Z", false, "2026-11-16T10:00:00.000Z"));
    assert_eq!(r.next().await, fire("2026-11-16T15:00:00.000Z", true, "2026-11-16T15:20:00.000Z"));
    assert_eq!(r.next().await, fire("2026-11-16T16:00:00.000Z", false, "2026-11-16T16:00:00.000Z"));
}

#[tokio::test(start_paused = true)]
async fn every_keeps_its_phase_across_restarts() {
    let home = tempfile::tempdir().unwrap();
    let mut r = start(source(None, Some("15m"), home.path()), "2026-11-16T09:00:00Z");
    assert_eq!(r.next().await, fire("2026-11-16T09:15:00.000Z", false, "2026-11-16T09:15:00.000Z"));
    r.stop.cancel();
    // Down from 09:16 to 10:05: one catch-up for 10:00, then 10:15.
    let mut r = start(source(None, Some("15m"), home.path()), "2026-11-16T10:05:00Z");
    assert_eq!(r.next().await, fire("2026-11-16T10:00:00.000Z", true, "2026-11-16T10:05:00.000Z"));
    assert_eq!(r.next().await, fire("2026-11-16T10:15:00.000Z", false, "2026-11-16T10:15:00.000Z"));
}

#[test]
fn latest_due_and_next_after() {
    let every = Plan::Every(Duration::from_mins(10));
    let a = ts("2026-11-16T09:00:00Z");
    assert_eq!(every.next_after(a, a), Some(ts("2026-11-16T09:10:00Z")));
    assert_eq!(every.next_after(a, ts("2026-11-16T09:25:00Z")), Some(ts("2026-11-16T09:30:00Z")));
    assert_eq!(every.latest_due(a, ts("2026-11-16T09:09:59Z")), None);
    assert_eq!(every.latest_due(a, ts("2026-11-16T10:05:00Z")), Some(ts("2026-11-16T10:00:00Z")));
    // Far from anything: no fire rather than an overflow.
    let huge = Plan::Every(Duration::from_millis(i64::MAX as u64));
    assert_eq!(huge.next_after(a, a), None);
    assert_eq!(huge.latest_due(a, ts("2026-11-16T10:05:00Z")), None);
    let tiny = Plan::Every(Duration::from_millis(1));
    assert_eq!(tiny.next_after(Timestamp::from_millis(0), Timestamp::from_millis(i64::MAX)), None);
    assert_eq!(
        tiny.latest_due(Timestamp::from_millis(i64::MIN), Timestamp::from_millis(i64::MAX)),
        None
    );
    // A minutely cron missed for a year: bounded work, the right answer.
    let minutely =
        Plan::Cron { cron: Cron::parse("* * * * *").unwrap(), zone: Zone::Named(chrono_tz::UTC) };
    assert_eq!(
        minutely.latest_due(ts("2025-11-16T09:00:00Z"), ts("2026-11-16T09:00:30Z")),
        Some(ts("2026-11-16T09:00:00Z"))
    );
}

#[test]
fn config_errors() {
    let home = std::path::Path::new("/tmp");
    let env = SourceEnv { home: home.into(), pulse_home: home.into() };
    let bad = [
        ScheduleSourceSettings { cron: Some("61 * * * *".into()), every: None, timezone: None },
        ScheduleSourceSettings {
            cron: Some("0 9 * * *".into()),
            every: None,
            timezone: Some("Mars/Olympus".into()),
        },
        ScheduleSourceSettings { cron: Some("0 0 31 2 *".into()), every: None, timezone: None },
        ScheduleSourceSettings { cron: None, every: None, timezone: None },
        // Typos that used to overflow (a panic in debug builds, a wrong schedule in release).
        ScheduleSourceSettings {
            cron: Some("5/4294967295 * * * *".into()),
            every: None,
            timezone: None,
        },
        ScheduleSourceSettings { cron: Some("*/90 * * * *".into()), every: None, timezone: None },
        ScheduleSourceSettings {
            cron: None,
            every: Some(Duration::from_millis(u64::MAX / 2)),
            timezone: None,
        },
    ];
    for s in bad {
        assert!(matches!(ScheduleSource::new("x", &s, &env), Err(SourceError::Config(_))), "{s:?}");
    }
}
