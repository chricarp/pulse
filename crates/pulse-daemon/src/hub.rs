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
//! - Each committed event travels with the deliveries recorded with it, so a live view shows
//!   what an event caused without asking. Deliveries recorded later for an earlier event (a
//!   debounce elapsing) travel with that event too, marked as not new: subscribers that match it
//!   send only the outcomes.
//! - Nothing is delivered on an RPC subscription before the subscribe call has been answered
//!   (`ready`), so a client always learns a subscription id before its first notification.
//! - Run output is not pushed through a queue at all. Each output subscription keeps a cursor
//!   and reads pages from the store when the run's [`watch`] counter moves, so a slow reader just
//!   reads bigger pages later.
//!
//! When nothing is pending, a subscription task sleeps on its channel with no timer armed.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pulse_contracts::rpc::{
    EventOutcome, EventsBatch, EventsBatchWithOutcomes, RunFilter, RunsChanged, StateChanged,
};
use pulse_contracts::{Delivery, Event, EventId, RunId, RunSummary, SubscriptionId};
use pulse_core::filter::CompiledFilter;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) const BROADCAST_CAPACITY: usize = 256;
pub(crate) const MAX_BATCH: usize = 200;
pub(crate) const FLUSH_EVERY: Duration = Duration::from_millis(250);
pub(crate) const STUCK_LIMIT: Duration = Duration::from_secs(30);

/// A committed event and the deliveries recorded for it in the same commit. `new` is false when
/// the event was published before and only `outcomes` are news (a debounce elapsed).
#[derive(Debug)]
pub(crate) struct LiveEvent {
    event: Event,
    outcomes: Vec<EventOutcome>,
    new: bool,
}

impl LiveEvent {
    /// The event, if it is new to the log (not just new outcomes for an earlier one). MCP
    /// wakeups look only at this: agents are never told outcomes, and a late outcome adds
    /// nothing to the log they read.
    pub(crate) fn new_event(&self) -> Option<&Event> {
        self.new.then_some(&self.event)
    }
}

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
    events: broadcast::Sender<Arc<[LiveEvent]>>,
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
    /// Publishes one commit: its new events, and `earlier` events that got deliveries in it.
    /// `runs` are the runs the commit created, for the status their outcomes carry.
    pub(crate) fn publish_events(
        &self,
        events: Vec<Event>,
        deliveries: &[Delivery],
        runs: &[RunSummary],
        earlier: Vec<Event>,
    ) {
        let status = |id: RunId| runs.iter().find(|r| r.id == id).map(|r| r.status);
        let mut by_event: HashMap<EventId, Vec<EventOutcome>> = HashMap::new();
        for d in deliveries {
            let outcome = EventOutcome::new(d, d.run.and_then(status));
            by_event.entry(d.event).or_default().push(outcome);
        }
        let mut items = Vec::with_capacity(events.len() + earlier.len());
        for event in events {
            let outcomes = by_event.remove(&event.id).unwrap_or_default();
            items.push(LiveEvent { event, outcomes, new: true });
        }
        for event in earlier {
            if let Some(outcomes) = by_event.remove(&event.id) {
                items.push(LiveEvent { event, outcomes, new: false });
            }
        }
        if !items.is_empty() {
            let _ = self.events.send(items.into());
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

    /// Drops the watch of a run that has already ended.
    pub(crate) fn forget_output(&self, run: RunId) {
        self.output_done(run);
    }

    /// Every committed batch, for subscribers that do their own filtering (MCP wakeups).
    pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<Arc<[LiveEvent]>> {
        self.events.subscribe()
    }

    /// Delivers events matching `filter`, with their outcomes, once `ready` is cancelled. Only
    /// for clients holding the daemon token: MCP agents get wakeups instead.
    pub(crate) fn spawn_event_subscription(
        &self,
        id: SubscriptionId,
        filter: CompiledFilter,
        deliver: impl FnMut(&EventsBatchWithOutcomes) -> Sent + Send + 'static,
        ready: CancellationToken,
        stop: CancellationToken,
        client: CancellationToken,
    ) {
        let rx = self.events.subscribe();
        let buffer =
            EventBuffer { id, filter, events: VecDeque::new(), late: VecDeque::new(), dropped: 0 };
        tokio::spawn(pump(rx, buffer, deliver, ready, stop, client));
    }

    pub(crate) fn spawn_run_subscription(
        &self,
        id: SubscriptionId,
        filter: RunFilter,
        deliver: impl FnMut(&RunsChanged) -> Sent + Send + 'static,
        ready: CancellationToken,
        stop: CancellationToken,
        client: CancellationToken,
    ) {
        let rx = self.runs.subscribe();
        let buffer = RunBuffer { id, filter, runs: BTreeMap::new() };
        tokio::spawn(pump(rx, buffer, deliver, ready, stop, client));
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
    /// New events with their outcomes. Dropping an event drops its outcomes (the log has them).
    events: VecDeque<(Event, Vec<EventOutcome>)>,
    /// Outcomes for events delivered in earlier batches.
    late: VecDeque<EventOutcome>,
    dropped: u64,
}

impl EventBuffer {
    fn push(&mut self, event: Event, outcomes: Vec<EventOutcome>) {
        if self.events.len() == MAX_BATCH {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.events.push_back((event, outcomes));
    }

    fn push_late(&mut self, outcomes: impl IntoIterator<Item = EventOutcome>) {
        for o in outcomes {
            // Bounded like events. Losing one only delays a marker: the run it created is
            // reported by `runs.changed`, and the log has the delivery.
            if self.late.len() == MAX_BATCH {
                self.late.pop_front();
            }
            self.late.push_back(o);
        }
    }
}

impl Buffer for EventBuffer {
    type Item = LiveEvent;
    type Batch = EventsBatchWithOutcomes;

    fn add(&mut self, items: &[LiveEvent]) {
        for item in items {
            let e = &item.event;
            if !self.filter.matches(e) {
                continue;
            }
            if item.new {
                self.push(e.clone(), item.outcomes.clone());
            } else {
                self.push_late(item.outcomes.iter().cloned());
            }
        }
    }

    fn lagged(&mut self, batches: u64) {
        // How many of the lost events would have matched is unknown; count the batches, so the
        // client knows the live view is incomplete and can query the log.
        self.dropped += batches;
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty() && self.late.is_empty() && self.dropped == 0
    }

    fn take(&mut self) -> EventsBatchWithOutcomes {
        let mut events = Vec::with_capacity(self.events.len());
        let mut outcomes = Vec::new();
        for (e, o) in self.events.drain(..) {
            events.push(e);
            outcomes.extend(o);
        }
        outcomes.extend(self.late.drain(..));
        EventsBatchWithOutcomes {
            batch: EventsBatch {
                subscription: self.id,
                events,
                dropped: std::mem::take(&mut self.dropped),
            },
            outcomes,
        }
    }

    fn restore(&mut self, batch: EventsBatchWithOutcomes) {
        let mut by_event: HashMap<EventId, Vec<EventOutcome>> = HashMap::new();
        for o in batch.outcomes {
            by_event.entry(o.event).or_default().push(o);
        }
        let newer = std::mem::take(&mut self.events);
        let newer_late = std::mem::take(&mut self.late);
        self.dropped += batch.batch.dropped;
        for e in batch.batch.events {
            let o = by_event.remove(&e.id).unwrap_or_default();
            self.push(e, o);
        }
        self.push_late(by_event.into_values().flatten());
        for (e, o) in newer {
            self.push(e, o);
        }
        self.push_late(newer_late);
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
    ready: CancellationToken,
    stop: CancellationToken,
    client: CancellationToken,
) {
    let mut last_flush: Option<Instant> = None;
    let mut full_since: Option<Instant> = None;
    loop {
        // Armed only while something is buffered: an idle subscription has no timer.
        let flush_at = (ready.is_cancelled() && !buffer.is_empty())
            .then(|| last_flush.map_or_else(Instant::now, |t| t + FLUSH_EVERY));
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = client.cancelled() => return,
            _ = ready.cancelled(), if !ready.is_cancelled() => {}
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use pulse_contracts::rpc::EventFilter;
    use pulse_contracts::{
        DeliveryId, DeliveryOutcome, EventType, NewEvent, SourceKind, SourceRef, Timestamp, Ulid,
    };

    use super::*;

    fn event(n: u64) -> Event {
        NewEvent {
            type_: EventType::external("t", "x").unwrap(),
            source: SourceRef::new(SourceKind::Ext, "t"),
            subject: Some(n.to_string()),
            time: Timestamp::from_millis(0),
            data: serde_json::json!({}),
            dedupe_key: None,
            coalesced: None,
            causation_run: None,
        }
        .into_event(EventId::from_ulid(Ulid::from_parts(n, 0)), None, None)
    }

    fn fired(e: &Event, n: u64) -> Delivery {
        Delivery {
            id: DeliveryId::from_ulid(Ulid::from_parts(n, 0)),
            event: e.id,
            rule: "r".into(),
            agent: "a".into(),
            at: Timestamp::from_millis(0),
            outcome: DeliveryOutcome::Fired,
            run: None,
        }
    }

    type Got = Arc<StdMutex<Vec<EventsBatchWithOutcomes>>>;

    fn subscribe(hub: &Hub, ready: CancellationToken) -> Got {
        let got: Got = Arc::default();
        let sink = got.clone();
        let filter = CompiledFilter::compile(&EventFilter::default()).unwrap();
        hub.spawn_event_subscription(
            SubscriptionId::from_ulid(Ulid::from_parts(1, 1)),
            filter,
            move |b: &EventsBatchWithOutcomes| {
                sink.lock().unwrap().push(b.clone());
                Sent::Ok
            },
            ready,
            CancellationToken::new(),
            CancellationToken::new(),
        );
        got
    }

    /// Lets spawned tasks run and timers fire (the clock is paused).
    async fn settle() {
        for _ in 0..10 {
            tokio::time::sleep(FLUSH_EVERY).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_delivered_before_the_subscription_is_ready() {
        let hub = Hub::default();
        let ready = CancellationToken::new();
        let got = subscribe(&hub, ready.clone());
        tokio::task::yield_now().await;
        let e = event(1);
        hub.publish_events(vec![e.clone()], &[fired(&e, 1)], &[], vec![]);
        settle().await;
        assert!(got.lock().unwrap().is_empty(), "held until the subscribe call is answered");
        ready.cancel();
        settle().await;
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].batch.events, std::slice::from_ref(&e));
        assert_eq!(got[0].outcomes.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn outcomes_follow_their_events_and_never_reach_agent_wakeups() {
        let hub = Hub::default();
        let ready = CancellationToken::new();
        ready.cancel();
        let full = subscribe(&hub, ready);
        // What MCP wakeups read: agents only ever look at new events, never at outcomes.
        let mut agent = hub.subscribe_events();
        tokio::task::yield_now().await;
        let (a, b) = (event(1), event(2));
        hub.publish_events(vec![a.clone(), b.clone()], &[fired(&b, 1)], &[], vec![]);
        settle().await;
        // A debounce elapsing later: only the outcome is news.
        hub.publish_events(vec![], &[fired(&a, 2)], &[], vec![a.clone()]);
        settle().await;
        let full = full.lock().unwrap();
        let events: Vec<_> =
            full.iter().flat_map(|x| x.batch.events.iter().map(|e| e.id)).collect();
        assert_eq!(events, [a.id, b.id], "an earlier event is not sent again");
        let outcomes: Vec<_> =
            full.iter().flat_map(|x| x.outcomes.iter().map(|o| o.event)).collect();
        assert_eq!(outcomes, [b.id, a.id]);
        let mut seen = Vec::new();
        while let Ok(batch) = agent.try_recv() {
            seen.extend(batch.iter().filter_map(|l| l.new_event()).map(|e| e.id));
        }
        assert_eq!(seen, [a.id, b.id], "a late outcome is not a new event for agents");
    }
}
