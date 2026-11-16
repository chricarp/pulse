//! Fan-out to live subscribers, bounded everywhere so that no client can make the daemon wait
//! or grow memory.
//!
//! - The engine publishes each committed batch once, to a bounded `broadcast` channel. It never
//!   waits: a subscriber that falls more than [`BROADCAST_CAPACITY`] batches behind loses the
//!   oldest batches and counts them as dropped.
//! - Each subscription is a task that filters what it receives into a small buffer (the newest
//!   [`MAX_BATCH`] entries) and delivers at most one batch per [`FLUSH_EVERY`] with a count of
//!   what was left out. Delivery never waits either: if the client's queue is full, the buffer
//!   keeps summarizing, and a client stuck for [`STUCK_LIMIT`] is disconnected.
//! - Run output is not pushed through a queue at all. Each output subscription keeps a cursor
//!   and reads pages from the store when the run's [`watch`] counter moves, so a slow reader just
//!   reads bigger pages later.
//!
//! When nothing is pending, a subscription task sleeps on its channel with no timer armed.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pulse_contracts::rpc::{EventsBatch, RunFilter, RunsChanged, StateChanged};
use pulse_contracts::{Event, RunId, RunSummary, SubscriptionId};
use pulse_core::filter::CompiledFilter;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) const BROADCAST_CAPACITY: usize = 256;
pub(crate) const MAX_BATCH: usize = 200;
pub(crate) const FLUSH_EVERY: Duration = Duration::from_millis(250);
pub(crate) const STUCK_LIMIT: Duration = Duration::from_secs(30);

/// What a delivery attempt did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sent {
    Ok,
    /// The client's queue is full; keep the batch and retry.
    Full,
    /// The client is gone; end the subscription.
    Closed,
}

#[derive(Debug)]
pub(crate) struct Hub {
    events: broadcast::Sender<Arc<[Event]>>,
    runs: broadcast::Sender<Arc<[RunSummary]>>,
    state: broadcast::Sender<Arc<StateChanged>>,
    outputs: Mutex<HashMap<RunId, watch::Sender<u64>>>,
}

impl Default for Hub {
    fn default() -> Self {
        Hub {
            events: broadcast::channel(BROADCAST_CAPACITY).0,
            runs: broadcast::channel(BROADCAST_CAPACITY).0,
            state: broadcast::channel(16).0,
            outputs: Mutex::new(HashMap::new()),
        }
    }
}

impl Hub {
    pub(crate) fn publish_events(&self, events: Vec<Event>) {
        if !events.is_empty() {
            let _ = self.events.send(events.into());
        }
    }

    pub(crate) fn publish_runs(&self, runs: Vec<RunSummary>) {
        if runs.is_empty() {
            return;
        }
        for r in &runs {
            if r.status.is_terminal() {
                self.output_done(r.id);
            }
        }
        let _ = self.runs.send(runs.into());
    }

    pub(crate) fn publish_state(&self, state: StateChanged) {
        let _ = self.state.send(Arc::new(state));
    }

    pub(crate) fn subscribe_state(&self) -> broadcast::Receiver<Arc<StateChanged>> {
        self.state.subscribe()
    }

    fn outputs(&self) -> std::sync::MutexGuard<'_, HashMap<RunId, watch::Sender<u64>>> {
        self.outputs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A counter that moves whenever `run` gets output, and closes when it ends.
    pub(crate) fn output_watch(&self, run: RunId) -> watch::Receiver<u64> {
        self.outputs().entry(run).or_insert_with(|| watch::channel(0).0).subscribe()
    }

    pub(crate) fn output_appended(&self, run: RunId) {
        if let Some(tx) = self.outputs().get(&run) {
            tx.send_modify(|n| *n += 1);
        }
    }

    fn output_done(&self, run: RunId) {
        // Dropping the sender wakes every receiver for a final read.
        self.outputs().remove(&run);
    }

    /// Delivers events matching `filter` (and `permit`, for scoped subscribers).
    pub(crate) fn spawn_event_subscription(
        &self,
        id: SubscriptionId,
        filter: CompiledFilter,
        permit: Option<Box<dyn Fn(&Event) -> bool + Send>>,
        deliver: impl FnMut(&EventsBatch) -> Sent + Send + 'static,
        stop: CancellationToken,
        client: CancellationToken,
    ) {
        let rx = self.events.subscribe();
        let buffer = EventBuffer { id, filter, permit, events: VecDeque::new(), dropped: 0 };
        tokio::spawn(pump(rx, buffer, deliver, stop, client));
    }

    pub(crate) fn spawn_run_subscription(
        &self,
        id: SubscriptionId,
        filter: RunFilter,
        deliver: impl FnMut(&RunsChanged) -> Sent + Send + 'static,
        stop: CancellationToken,
        client: CancellationToken,
    ) {
        let rx = self.runs.subscribe();
        let buffer = RunBuffer { id, filter, runs: BTreeMap::new() };
        tokio::spawn(pump(rx, buffer, deliver, stop, client));
    }
}

/// The per-subscription state between flushes.
trait Buffer: Send + 'static {
    type Item: Send + Sync + 'static;
    type Batch: Send + 'static;
    fn add(&mut self, items: &[Self::Item]);
    fn lagged(&mut self, batches: u64);
    fn is_empty(&self) -> bool;
    /// The batch to deliver; the buffer is left empty.
    fn take(&mut self) -> Self::Batch;
    /// Puts back a batch that could not be delivered.
    fn restore(&mut self, batch: Self::Batch);
}

struct EventBuffer {
    id: SubscriptionId,
    filter: CompiledFilter,
    permit: Option<Box<dyn Fn(&Event) -> bool + Send>>,
    events: VecDeque<Event>,
    dropped: u64,
}

impl Buffer for EventBuffer {
    type Item = Event;
    type Batch = EventsBatch;

    fn add(&mut self, items: &[Event]) {
        for e in items {
            if self.filter.matches(e) && self.permit.as_ref().is_none_or(|p| p(e)) {
                if self.events.len() == MAX_BATCH {
                    self.events.pop_front();
                    self.dropped += 1;
                }
                self.events.push_back(e.clone());
            }
        }
    }

    fn lagged(&mut self, batches: u64) {
        // How many of the lost events would have matched is unknown; count the batches, so the
        // client knows the live view is incomplete and can query the log.
        self.dropped += batches;
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty() && self.dropped == 0
    }

    fn take(&mut self) -> EventsBatch {
        EventsBatch {
            subscription: self.id,
            events: self.events.drain(..).collect(),
            dropped: std::mem::take(&mut self.dropped),
        }
    }

    fn restore(&mut self, batch: EventsBatch) {
        self.dropped += batch.dropped;
        let mut events: VecDeque<Event> = batch.events.into();
        events.extend(self.events.drain(..));
        while events.len() > MAX_BATCH {
            events.pop_front();
            self.dropped += 1;
        }
        self.events = events;
    }
}

struct RunBuffer {
    id: SubscriptionId,
    filter: RunFilter,
    /// Latest state per run: a run that changed twice since the last flush is sent once.
    runs: BTreeMap<RunId, RunSummary>,
}

pub(crate) fn run_matches(f: &RunFilter, r: &RunSummary) -> bool {
    f.rule.as_ref().is_none_or(|x| r.rule.as_ref() == Some(x))
        && f.agent.as_ref().is_none_or(|x| *x == r.agent)
        && (f.status.is_empty() || f.status.contains(&r.status))
        && f.event.is_none_or(|e| e == r.event)
}

impl Buffer for RunBuffer {
    type Item = RunSummary;
    type Batch = RunsChanged;

    fn add(&mut self, items: &[RunSummary]) {
        for r in items {
            if run_matches(&self.filter, r) {
                self.runs.insert(r.id, r.clone());
                if self.runs.len() > MAX_BATCH {
                    self.runs.pop_first();
                }
            }
        }
    }

    fn lagged(&mut self, _: u64) {}

    fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    fn take(&mut self) -> RunsChanged {
        RunsChanged {
            subscription: self.id,
            runs: std::mem::take(&mut self.runs).into_values().collect(),
        }
    }

    fn restore(&mut self, batch: RunsChanged) {
        for r in batch.runs {
            self.runs.entry(r.id).or_insert(r);
        }
        while self.runs.len() > MAX_BATCH {
            self.runs.pop_first();
        }
    }
}

async fn pump<B: Buffer>(
    mut rx: broadcast::Receiver<Arc<[B::Item]>>,
    mut buffer: B,
    mut deliver: impl FnMut(&B::Batch) -> Sent,
    stop: CancellationToken,
    client: CancellationToken,
) {
    let mut last_flush: Option<Instant> = None;
    let mut full_since: Option<Instant> = None;
    loop {
        // Armed only while something is buffered: an idle subscription has no timer.
        let flush_at = (!buffer.is_empty())
            .then(|| last_flush.map_or_else(Instant::now, |t| t + FLUSH_EVERY));
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = client.cancelled() => return,
            received = rx.recv() => match received {
                Ok(items) => buffer.add(&items),
                Err(broadcast::error::RecvError::Lagged(n)) => buffer.lagged(n),
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = sleep_until(flush_at) => {
                let now = Instant::now();
                last_flush = Some(now);
                let batch = buffer.take();
                match deliver(&batch) {
                    Sent::Ok => full_since = None,
                    Sent::Full => {
                        buffer.restore(batch);
                        let since = *full_since.get_or_insert(now);
                        if now.duration_since(since) >= STUCK_LIMIT {
                            tracing::info!("disconnecting a client that stopped reading");
                            client.cancel();
                            return;
                        }
                    }
                    Sent::Closed => return,
                }
            }
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
