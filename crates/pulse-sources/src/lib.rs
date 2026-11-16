//! Sources: the `Source` trait, the context a running source gets, and the built-in sources.
//!
//! ## Contract
//!
//! - A source is built from its config by [`build`], then [`Source::run`] is awaited until the
//!   context's `shutdown` token is cancelled (return `Ok(())`) or a fatal error (return `Err`,
//!   after reporting it through `health`).
//! - Events go to the engine through [`EventSink::send`], a **bounded** channel: when the
//!   engine is slow, `send` waits. Keep coalescing while you wait; never buffer without limit.
//! - Emit built-in events with [`pulse_contracts::catalog::BuiltinEvent::into_new_event`], so
//!   type and subject follow the catalog. Paths must be clean absolute paths (no `.`, `..`,
//!   `//`, trailing slash): `pulse-core` treats anything else as out of every scope.
//! - Report state with [`HealthReporter`]: `healthy()` once watching, `degraded(..)` for
//!   recoverable trouble, `error(..)` to record a problem without changing state, `failed(..)`
//!   before returning an error. The daemon shows this in the UI.
//! - Sources know nothing about rules or agents. Platform quirks stay inside the source.
//! - Time-based logic (quiet periods, storm folding) should be a pure state machine driven by
//!   explicit timestamps and one timer, so it can be tested without sleeping.
//!
//! The process wrapper (`pulse run`) and ingest parsing are not `Source`s; they live in
//! [`process`] and [`ingest`].

pub mod calendar;
pub mod clock;
pub mod fs;
pub mod git;
pub mod ingest;
pub mod process;
pub mod schedule;
mod state;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use pulse_contracts::config::{SourceConfig, SourceSettings};
use pulse_contracts::status::{SourceErrorInfo, SourceHealth, SourceState};
use pulse_contracts::{NewEvent, SourceRef, Timestamp};
use tokio::sync::{mpsc, watch};
pub use tokio_util::sync::CancellationToken;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A running producer of events.
pub trait Source: Send + 'static {
    /// `pulse://<kind>/<name>`.
    fn source_ref(&self) -> &SourceRef;

    /// Watches until `cx.shutdown` is cancelled or a fatal error occurs.
    fn run(self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>>;
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("invalid source config: {0}")]
    Config(String),
    #[error("{context}: {source}")]
    Io { context: String, source: std::io::Error },
    #[error("{0}")]
    Platform(String),
    #[error("the engine stopped accepting events")]
    SinkClosed,
    #[error("the {0} source is not implemented yet")]
    NotImplemented(&'static str),
}

/// What sources need from the environment, passed in rather than read from it.
#[derive(Clone, Debug)]
pub struct SourceEnv {
    /// Expands `~` in config paths.
    pub home: PathBuf,
    /// For caches a source may keep (for example a calendar's last download).
    pub pulse_home: PathBuf,
}

/// Builds a source from its config. Errors here are config errors the daemon reports.
pub fn build(config: &SourceConfig, env: &SourceEnv) -> Result<Box<dyn Source>, SourceError> {
    Ok(match &config.settings {
        SourceSettings::Fs(s) => Box::new(fs::FsSource::new(&config.name, s, env)?),
        SourceSettings::Git(s) => Box::new(git::GitSource::new(&config.name, s, env)?),
        SourceSettings::Schedule(s) => {
            Box::new(schedule::ScheduleSource::new(&config.name, s, env)?)
        }
        SourceSettings::Calendar(s) => {
            Box::new(calendar::CalendarSource::new(&config.name, s, env)?)
        }
    })
}

/// Everything a running source gets.
#[derive(Debug)]
pub struct SourceContext {
    pub events: EventSink,
    pub shutdown: CancellationToken,
    pub health: HealthReporter,
}

/// Builds a context and the engine's ends of it: the event receiver and the health watcher.
pub fn context(
    source: SourceRef,
    capacity: usize,
    shutdown: CancellationToken,
    now: Timestamp,
) -> (SourceContext, mpsc::Receiver<NewEvent>, watch::Receiver<SourceHealth>) {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    let (htx, hrx) = watch::channel(SourceHealth::starting(now));
    let health = HealthReporter { source: source.clone(), tx: Arc::new(htx) };
    let cx = SourceContext { events: EventSink { tx, health: health.clone() }, shutdown, health };
    (cx, rx, hrx)
}

/// The bounded sink for events. Cloneable, so a source may send from several tasks.
#[derive(Clone, Debug)]
pub struct EventSink {
    tx: mpsc::Sender<NewEvent>,
    health: HealthReporter,
}

impl EventSink {
    /// Sends one event, waiting while the engine is behind. Counts it in the source's health.
    pub async fn send(&self, event: NewEvent) -> Result<(), SourceError> {
        let at = event.time;
        self.tx.send(event).await.map_err(|_| SourceError::SinkClosed)?;
        self.health.record_event(at);
        Ok(())
    }

    /// Waits for room in the channel without giving up an event. Use this in a `select!` next to
    /// other work: unlike `send`, dropping the future loses nothing, so a source can keep
    /// coalescing while the engine is behind and send once there is room.
    pub async fn reserve(&self) -> Result<SinkPermit<'_>, SourceError> {
        let permit = self.tx.reserve().await.map_err(|_| SourceError::SinkClosed)?;
        Ok(SinkPermit { permit, health: &self.health })
    }

    /// Sends without waiting. `Err` gives the event back when the channel is full, so the
    /// caller can keep it in its (bounded) coalescing state.
    pub fn try_send(&self, event: NewEvent) -> Result<(), TrySendError> {
        let at = event.time;
        match self.tx.try_send(event) {
            Ok(()) => {
                self.health.record_event(at);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(e)) => Err(TrySendError::Full(Box::new(e))),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(TrySendError::Closed),
        }
    }
}

/// Room for one event, from [`EventSink::reserve`].
#[derive(Debug)]
pub struct SinkPermit<'a> {
    permit: mpsc::Permit<'a, NewEvent>,
    health: &'a HealthReporter,
}

impl SinkPermit<'_> {
    pub fn send(self, event: NewEvent) {
        let at = event.time;
        self.permit.send(event);
        self.health.record_event(at);
    }
}

#[derive(Debug)]
pub enum TrySendError {
    Full(Box<NewEvent>),
    Closed,
}

/// Publishes a source's health to the daemon (a `watch` channel: the daemon sees changes
/// without polling).
#[derive(Clone, Debug)]
pub struct HealthReporter {
    source: SourceRef,
    tx: Arc<watch::Sender<SourceHealth>>,
}

fn now() -> Timestamp {
    Timestamp::from_system_time(std::time::SystemTime::now())
}

impl HealthReporter {
    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    fn set_state(&self, state: SourceState, message: Option<String>) {
        self.tx.send_modify(|h| {
            if h.state != state {
                h.since = now();
            }
            h.state = state;
            h.message = message;
        });
    }

    pub fn healthy(&self) {
        self.set_state(SourceState::Healthy, None);
    }

    pub fn degraded(&self, message: impl Into<String>) {
        let message = message.into();
        self.error(message.clone());
        self.set_state(SourceState::Degraded, Some(message));
    }

    /// Records a problem without changing state.
    pub fn error(&self, message: impl Into<String>) {
        let info = SourceErrorInfo { at: now(), message: message.into() };
        self.tx.send_modify(|h| h.last_error = Some(info));
    }

    pub fn failed(&self, message: impl Into<String>) {
        let message = message.into();
        self.error(message.clone());
        self.set_state(SourceState::Failed, Some(message));
    }

    pub fn stopped(&self) {
        self.set_state(SourceState::Stopped, None);
    }

    fn record_event(&self, at: Timestamp) {
        self.tx.send_modify(|h| {
            h.events_emitted += 1;
            h.last_event_at = Some(at);
        });
    }
}

#[cfg(test)]
mod tests {
    use pulse_contracts::SourceKind;
    use pulse_contracts::catalog::{BuiltinEvent, FsFileData};

    use super::*;

    #[tokio::test]
    async fn sink_is_bounded_and_counts_events() {
        let source = SourceRef::new(SourceKind::Fs, "u");
        let (cx, mut rx, health) =
            context(source.clone(), 1, CancellationToken::new(), Timestamp::EPOCH);
        let ev = |n: i64| {
            BuiltinEvent::FsFileCreated(FsFileData {
                path: format!("/u/{n}"),
                dir: "/u".into(),
                name: n.to_string(),
                ext: None,
                size: Some(1),
            })
            .into_new_event(source.clone(), Timestamp::from_millis(n))
        };
        cx.events.try_send(ev(1)).unwrap();
        assert!(matches!(cx.events.try_send(ev(2)), Err(TrySendError::Full(_))));
        assert_eq!(rx.recv().await.unwrap().subject.as_deref(), Some("/u/1"));
        cx.events.send(ev(3)).await.unwrap();
        assert_eq!(health.borrow().events_emitted, 2);
        assert_eq!(health.borrow().last_event_at, Some(Timestamp::from_millis(3)));
        cx.health.degraded("inotify watch limit reached");
        assert_eq!(health.borrow().state, SourceState::Degraded);
        assert!(health.borrow().last_error.is_some());
        drop(rx);
        assert!(matches!(cx.events.send(ev(4)).await, Err(SourceError::SinkClosed)));
    }
}
