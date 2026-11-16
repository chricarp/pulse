//! Subscriptions for agents that are already running.
//!
//! **The log is the queue.** A subscription is a saved filter owned by one agent plus a wakeup:
//! a live backend subscription whose batches only say "something matching arrived". Events are
//! always read from the log with [`read_after`], after a cursor the caller passes back (the id
//! of the last event it received). So:
//!
//! - Nothing is buffered per subscription; there is no queue to overflow and nothing to drop.
//!   A slow agent is simply behind in the log, and catches up page by page (`has_more`).
//! - Delivery is at-least-once with no acknowledgements: the caller's cursor is its
//!   acknowledgement. A result lost in transit (a client-side tool timeout) is re-read with the
//!   same cursor.
//! - A subscription can be resumed without server memory: after a restart or an expiry,
//!   `subscribe` again and keep using the last cursor. Within retention, nothing is missed.
//! - Every event an agent receives came from [`Backend::query_events`] as that agent, so it
//!   never sees an event the backend would not give it.
//!
//! The registry is bounded per agent and in total, and an entry nobody has used for
//! [`Limits::subscription_idle_ttl`] is removed by its own timer (one timer per entry, armed
//! for its deadline; no sweeping loop).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use pulse_contracts::rpc::{EventFilter, EventQuery, EventsBatch, Order};
use pulse_contracts::{Event, EventId, SubscriptionId};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::{AgentIdentity, Backend, BackendError, Clock, Limits};

/// Pages of out-of-scope rows one read follows before handing back an advanced cursor.
const MAX_EMPTY_PAGES: u32 = 8;

/// Events after a cursor, as the agent may see them.
#[derive(Debug)]
pub(crate) struct Read {
    pub events: Vec<Event>,
    /// Pass back to continue. Past every returned event, and past rows the agent cannot see.
    pub cursor: EventId,
    /// More may be ready right now.
    pub has_more: bool,
}

/// The one "what happened after this cursor" function behind `wait_for_events` and SEP-3415
/// poll and push. Oldest first, at most `max` events.
pub(crate) async fn read_after<B: Backend>(
    backend: &B,
    agent: &AgentIdentity,
    filter: &EventFilter,
    cursor: EventId,
    max: u32,
) -> Result<Read, BackendError> {
    let mut cursor = cursor;
    for _ in 0..MAX_EMPTY_PAGES {
        let query = EventQuery {
            filter: filter.clone(),
            cursor: Some(cursor),
            limit: Some(max),
            order: Order::OldestFirst,
        };
        let page = backend.query_events(agent, query).await?;
        // A backend returning ids at or before the cursor would repeat events forever.
        let events: Vec<Event> = page.events.into_iter().filter(|e| e.id > cursor).collect();
        let next = page.next_cursor.filter(|&c| c > cursor);
        let advanced = events.last().map(|e| e.id).into_iter().chain(next).max().unwrap_or(cursor);
        if !events.is_empty() || next.is_none() {
            return Ok(Read { events, cursor: advanced, has_more: next.is_some() });
        }
        cursor = advanced;
    }
    Ok(Read { events: Vec::new(), cursor, has_more: true })
}

/// A cursor at the current end of the log as `agent` sees it: "from now". The newest row the
/// backend examined (visible or not), so nothing already logged counts as new. On an empty log,
/// the clock: every later event sorts after it.
pub(crate) async fn head<B: Backend>(
    backend: &B,
    agent: &AgentIdentity,
    clock: &Clock,
) -> Result<EventId, BackendError> {
    // Read before the query: an event logged after the query sorts after both.
    let now = clock.now_cursor();
    let query = EventQuery { limit: Some(1), order: Order::NewestFirst, ..EventQuery::default() };
    let page = backend.query_events(agent, query).await?;
    let newest = page.events.first().map(|e| e.id).into_iter().chain(page.next_cursor).max();
    Ok(newest.unwrap_or(now))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SubscribeError {
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("this agent already has {max} subscriptions")]
    AgentLimit { max: usize, existing: Vec<(SubscriptionId, EventFilter)> },
    #[error("the server has reached its subscription limit ({max}); try again later")]
    TotalLimit { max: usize },
}

/// Not found, or owned by another agent: the two are deliberately indistinguishable.
#[derive(Debug, thiserror::Error)]
#[error("unknown or expired subscription")]
pub(crate) struct UnknownSubscription;

pub(crate) struct Registry {
    inner: Arc<Mutex<HashMap<SubscriptionId, Entry>>>,
    limits: Limits,
}

struct Entry {
    owner: AgentIdentity,
    filter: EventFilter,
    /// Bumped once per live batch. The sender lives in the pump; when the backend closes the
    /// subscription the pump ends and waiters see the channel close.
    wake: watch::Receiver<u64>,
    last_used: Instant,
    waiting: u32,
    pump: JoinHandle<()>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        // Aborting the pump drops the backend receiver, which unsubscribes in the backend.
        self.pump.abort();
    }
}

/// A subscription as `wait_for_events` sees it while waiting. Dropping it marks the wait over
/// and restarts the idle clock.
pub(crate) struct Lease {
    inner: Weak<Mutex<HashMap<SubscriptionId, Entry>>>,
    id: SubscriptionId,
    pub filter: EventFilter,
    pub wake: watch::Receiver<u64>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade()
            && let Some(e) = lock(&inner).get_mut(&self.id)
        {
            e.waiting = e.waiting.saturating_sub(1);
            e.last_used = Instant::now();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock leaves plain data behind; keep serving.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Registry {
    pub fn new(limits: Limits) -> Self {
        Registry { inner: Arc::default(), limits }
    }

    /// Subscribes `agent` to `filter`. The same agent subscribing to the same filter again gets
    /// the existing subscription, so repeated calls do not accumulate.
    pub async fn subscribe<B: Backend>(
        &self,
        backend: &B,
        agent: &AgentIdentity,
        filter: EventFilter,
    ) -> Result<SubscriptionId, SubscribeError> {
        if let Some(id) = self.existing(agent, &filter)? {
            return Ok(id);
        }
        let live = backend.subscribe(agent, filter.clone()).await?;
        let mut map = lock(&self.inner);
        // Re-check: another call may have subscribed while the lock was released.
        if let Some(id) = find(&map, agent, &filter) {
            touch(&mut map, id);
            return Ok(id);
        }
        check_limits(&map, agent, &self.limits)?;
        let (wake_tx, wake) = watch::channel(0);
        let pump = tokio::spawn(pump(
            Arc::downgrade(&self.inner),
            live.id,
            live.events,
            wake_tx,
            self.limits.subscription_idle_ttl,
        ));
        map.insert(
            live.id,
            Entry {
                owner: agent.clone(),
                filter,
                wake,
                last_used: Instant::now(),
                waiting: 0,
                pump,
            },
        );
        Ok(live.id)
    }

    fn existing(
        &self,
        agent: &AgentIdentity,
        filter: &EventFilter,
    ) -> Result<Option<SubscriptionId>, SubscribeError> {
        let mut map = lock(&self.inner);
        if let Some(id) = find(&map, agent, filter) {
            touch(&mut map, id);
            return Ok(Some(id));
        }
        check_limits(&map, agent, &self.limits)?;
        Ok(None)
    }

    /// Ends a subscription. Only its owner can.
    pub fn unsubscribe(
        &self,
        agent: &AgentIdentity,
        id: SubscriptionId,
    ) -> Result<(), UnknownSubscription> {
        let mut map = lock(&self.inner);
        match map.get(&id) {
            Some(e) if e.owner == *agent => {
                map.remove(&id);
                Ok(())
            }
            _ => Err(UnknownSubscription),
        }
    }

    /// Starts a wait on a subscription `agent` owns. It does not expire while leased.
    pub fn lease(
        &self,
        agent: &AgentIdentity,
        id: SubscriptionId,
    ) -> Result<Lease, UnknownSubscription> {
        let mut map = lock(&self.inner);
        let e = map.get_mut(&id).filter(|e| e.owner == *agent).ok_or(UnknownSubscription)?;
        e.waiting += 1;
        e.last_used = Instant::now();
        let mut wake = e.wake.clone();
        wake.mark_unchanged();
        Ok(Lease { inner: Arc::downgrade(&self.inner), id, filter: e.filter.clone(), wake })
    }
}

fn find(
    map: &HashMap<SubscriptionId, Entry>,
    agent: &AgentIdentity,
    filter: &EventFilter,
) -> Option<SubscriptionId> {
    map.iter().find(|(_, e)| e.owner == *agent && e.filter == *filter).map(|(id, _)| *id)
}

fn touch(map: &mut HashMap<SubscriptionId, Entry>, id: SubscriptionId) {
    if let Some(e) = map.get_mut(&id) {
        e.last_used = Instant::now();
    }
}

fn check_limits(
    map: &HashMap<SubscriptionId, Entry>,
    agent: &AgentIdentity,
    limits: &Limits,
) -> Result<(), SubscribeError> {
    let mine: Vec<_> = map
        .iter()
        .filter(|(_, e)| e.owner == *agent)
        .map(|(id, e)| (*id, e.filter.clone()))
        .collect();
    if mine.len() >= limits.subscriptions_per_agent {
        return Err(SubscribeError::AgentLimit {
            max: limits.subscriptions_per_agent,
            existing: mine,
        });
    }
    if map.len() >= limits.subscriptions_total {
        return Err(SubscribeError::TotalLimit { max: limits.subscriptions_total });
    }
    Ok(())
}

/// Turns live batches into wakeups, and removes the entry when it has been idle for `ttl` or
/// the backend closes it.
async fn pump(
    registry: Weak<Mutex<HashMap<SubscriptionId, Entry>>>,
    id: SubscriptionId,
    mut events: mpsc::Receiver<EventsBatch>,
    wake: watch::Sender<u64>,
    ttl: std::time::Duration,
) {
    loop {
        let deadline = {
            let Some(inner) = registry.upgrade() else { return };
            let mut map = lock(&inner);
            let Some(e) = map.get(&id) else { return };
            let now = Instant::now();
            if e.waiting == 0 && now >= e.last_used + ttl {
                // Removing the entry aborts this task at its next await; there is none.
                map.remove(&id);
                return;
            }
            // While a wait holds a lease, check again a full ttl later.
            if e.waiting > 0 { now + ttl } else { e.last_used + ttl }
        };
        tokio::select! {
            batch = events.recv() => match batch {
                Some(_) => wake.send_modify(|n| *n = n.wrapping_add(1)),
                // The backend ended it. Waiters see `wake` close when this returns.
                None => {
                    if let Some(inner) = registry.upgrade() {
                        lock(&inner).remove(&id);
                    }
                    return;
                }
            },
            () = tokio::time::sleep_until(deadline) => {}
        }
    }
}
