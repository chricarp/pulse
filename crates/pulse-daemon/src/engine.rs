//! The engine: one task that owns the routing state. It takes new events from sources and from
//! ingest/report/replay, dedupes them, attributes causation, assigns ids, routes them and
//! commits each batch (events, deliveries, runs, outbox rows, rule state) in **one store
//! transaction**. Then it answers the callers, wakes the effect worker and fans the batch out.
//!
//! It arms exactly one timer, for the router's next debounce deadline. It never waits on a
//! subscriber (fan-out is a non-blocking broadcast) or on an agent (runs are the worker's). The
//! only thing it waits on is the store, which is the backpressure path: while a commit is in
//! flight, sources' bounded channels fill and their sends wait.
//!
//! Control messages (config, snooze, pause, run starts, writes and ends) have their own channel
//! and are all handled before the next batch of events, so attribution sees a run as soon as it
//! starts and a write its agent reported before the file event that write causes (which waits
//! out the fs source's quiet period).

use std::collections::BTreeSet;
use std::sync::Arc;

use pulse_contracts::config::Config;
use pulse_contracts::rpc::StateTopic;
use pulse_contracts::run::RunSnapshot;
use pulse_contracts::{
    BlockReason, Delivery, Event, EventId, Inference, NewEvent, Outcome, RunId, RunStatus,
    RunSummary, Timestamp,
};
use pulse_core::dedupe::Dedupe;
use pulse_core::limits::{FireHistory, history_capacity};
use pulse_core::{
    Cause, NonceSource, RecentRun, RouteContext, RouteMode, RouterState, RuleRuntime, RunWrites,
    attribute, route,
};
use pulse_store::{Commit, Effect, NewRun, RuleStateRow};
use tokio::sync::{mpsc, oneshot};

use crate::config::Snapshot;
use crate::error::{ServiceError, ServiceResult};
use crate::release;
use crate::shared::Shared;

/// Events (and requests carrying events) for the engine.
#[derive(Debug)]
pub(crate) enum Input {
    Source(NewEvent),
    /// Ingested or reported events. The reply has one entry per event: `None` if deduplicated.
    Ingest {
        events: Vec<NewEvent>,
        reply: oneshot::Sender<ServiceResult<Vec<Option<EventId>>>>,
    },
    Replay {
        event: NewEvent,
        replay_of: EventId,
        rules: Option<Vec<String>>,
        reply: oneshot::Sender<ServiceResult<(Event, Vec<Delivery>)>>,
    },
}

#[derive(Debug)]
pub(crate) enum Control {
    Config(Arc<Snapshot>),
    /// `Some(until)` snoozes; `None` unsnoozes (and closes a tripped breaker).
    Snooze {
        rule: String,
        until: Option<Timestamp>,
        reply: oneshot::Sender<ServiceResult<()>>,
    },
    Paused(bool),
    RunStarted(RecentRun),
    /// What the run's adapter has reported about its writes so far (replaces the old state).
    RunWrites {
        id: RunId,
        writes: RunWrites,
    },
    RunEnded {
        id: RunId,
        at: Timestamp,
    },
    /// Run states changed outside routing (approval, cancel, end, rerun): recount backlogs.
    RefreshBacklog,
    CopyState(oneshot::Sender<RouterState>),
}

#[derive(Clone, Debug)]
pub(crate) struct EngineHandle {
    pub control: mpsc::Sender<Control>,
    pub input: mpsc::Sender<Input>,
}

pub(crate) const INPUT_CAPACITY: usize = 1024;
const CONTROL_CAPACITY: usize = 256;
/// Events routed and committed together.
const BATCH: usize = 256;
/// Runs remembered for attribution: active runs, and ended ones within the grace period.
const MAX_RECENT_RUNS: usize = 1024;

pub(crate) fn channels() -> (EngineHandle, mpsc::Receiver<Control>, mpsc::Receiver<Input>) {
    let (ctx, crx) = mpsc::channel(CONTROL_CAPACITY);
    let (itx, irx) = mpsc::channel(INPUT_CAPACITY);
    (EngineHandle { control: ctx, input: itx }, crx, irx)
}

impl EngineHandle {
    pub(crate) async fn control(&self, msg: Control) -> ServiceResult<()> {
        self.control.send(msg).await.map_err(|_| ServiceError::shutting_down())
    }
    pub(crate) async fn input(&self, msg: Input) -> ServiceResult<()> {
        self.input.send(msg).await.map_err(|_| ServiceError::shutting_down())
    }
}

/// Per-run randomness for prompt delimiters, from the OS.
#[derive(Debug, Default)]
pub(crate) struct OsNonces;

impl NonceSource for OsNonces {
    fn nonce(&mut self) -> [u8; 16] {
        let mut n = [0u8; 16];
        if getrandom::fill(&mut n).is_err() {
            // The OS RNG failing is not recoverable in any useful way; a delimiter built from
            // the clock and a counter is still unknown to event content.
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            n = t.to_le_bytes();
        }
        n
    }
}

pub(crate) struct Engine {
    shared: Arc<Shared>,
    snapshot: Arc<Snapshot>,
    state: RouterState,
    dedupe: Dedupe,
    recent: Vec<RecentRun>,
    paused: bool,
    nonces: OsNonces,
}

/// A batch being assembled.
#[derive(Default)]
struct Pending {
    commit: Commit,
    /// Events from earlier commits that get deliveries in this one (debounces that elapsed).
    earlier: Vec<Event>,
    rules_touched: BTreeSet<String>,
    runs: Vec<RunSummary>,
    replies: Vec<Reply>,
    /// A circuit breaker tripped in this batch; clients are told after the commit.
    breaker_tripped: bool,
}

enum Reply {
    Ingest(oneshot::Sender<ServiceResult<Vec<Option<EventId>>>>, Vec<Option<EventId>>),
    Replay(oneshot::Sender<ServiceResult<(Event, Vec<Delivery>)>>, Box<Event>, Vec<Delivery>),
}

impl Engine {
    /// Builds the engine with router state rebuilt from the store: snooze and breaker from
    /// `rule_state`, recent fires from deliveries, backlogs from runs, the pause flag, the
    /// dedupe table, and the runs attribution still has to know about. `interrupted` are the
    /// runs that were still running when the previous daemon stopped.
    pub(crate) async fn restore(
        shared: Arc<Shared>,
        snapshot: Arc<Snapshot>,
        interrupted: &[RunId],
    ) -> ServiceResult<Engine> {
        let limits = &snapshot.config.limits;
        let mut engine = Engine {
            state: RouterState::new(limits),
            dedupe: Dedupe::new(limits.dedupe_ttl, limits.dedupe_capacity as usize),
            recent: Vec::new(),
            paused: shared.paused(),
            nonces: OsNonces,
            snapshot,
            shared,
        };
        engine.restore_rules().await?;
        engine.refresh_backlog().await?;
        engine.restore_dedupe().await?;
        engine.restore_recent_runs(interrupted).await?;
        Ok(engine)
    }

    /// Keys accepted within the TTL stay duplicates across a restart: an external source that
    /// retries after an outage must not fire twice.
    async fn restore_dedupe(&mut self) -> ServiceResult<()> {
        let limits = &self.snapshot.config.limits;
        let since = self.shared.clock.now().saturating_sub(limits.dedupe_ttl);
        let limit = limits.dedupe_capacity;
        let keys =
            self.shared.store.read(move |q| q.query().recent_dedupe_keys(since, limit)).await?;
        for (source, key, at) in keys {
            self.dedupe.check(&format!("{source}\0{key}"), at);
        }
        Ok(())
    }

    /// A new table, refilled from the log (with the current limits).
    async fn reset_dedupe(&mut self) {
        let limits = &self.snapshot.config.limits;
        self.dedupe = Dedupe::new(limits.dedupe_ttl, limits.dedupe_capacity as usize);
        if let Err(e) = self.restore_dedupe().await {
            tracing::error!("could not rebuild the dedupe table: {e}");
        }
    }

    /// Runs that ended within the grace period still attribute events, by inference only (their
    /// output is no longer read, so their writes are opaque). A run that was running
    /// when the daemon died may still be running (its agent is in its own process group and
    /// was not killed), and nothing enforces its timeout any more; it is treated as active
    /// until its timeout would have ended it, so an orphan's late writes are still its own.
    async fn restore_recent_runs(&mut self, interrupted: &[RunId]) -> ServiceResult<()> {
        let now = self.shared.clock.now();
        let since = now.saturating_sub(self.snapshot.config.limits.causation_grace);
        let limit = MAX_RECENT_RUNS as u32;
        let runs =
            self.shared.store.read(move |q| q.query().runs_ended_since(since, limit)).await?;
        for run in runs {
            let cwd = run.invocation.map(|i| i.cwd).filter(|c| !c.is_empty());
            let cwds = match cwd {
                Some(cwd) => {
                    let real = tokio::fs::canonicalize(&cwd).await.ok();
                    crate::worker::cwd_spellings(&cwd, real.as_ref().and_then(|r| r.to_str()))
                }
                None => Vec::new(),
            };
            let started_at = run.started_at.unwrap_or(run.created_at);
            let ended_at = if interrupted.contains(&run.id) {
                run.ended_at
                    .map(|end| end.max(started_at.saturating_add(run.snapshot.agent.timeout)))
            } else {
                run.ended_at
            };
            self.recent.push(RecentRun {
                id: run.id,
                rule: run.rule,
                cwds,
                started_at,
                ended_at,
                chain_depth: run.chain_depth,
                // Nobody reads its output any more.
                writes: RunWrites::opaque(Inference::Reloaded),
            });
        }
        Ok(())
    }

    async fn restore_rules(&mut self) -> ServiceResult<()> {
        let now = self.shared.clock.now();
        let rows = self.shared.store.read(|q| q.query().rule_states()).await?;
        let breaker = self.snapshot.config.limits.circuit_breaker.clone();
        for rule in &self.snapshot.config.rules {
            let r = &rule.config;
            let window = [
                r.cooldown.unwrap_or_default(),
                r.rate_limit.map(|x| x.per).unwrap_or_default(),
                breaker.window,
            ]
            .into_iter()
            .max()
            .unwrap_or_default();
            let capacity = history_capacity(r.rate_limit, &breaker);
            let name = r.name.clone();
            let since = now.saturating_sub(window);
            let limit = u32::try_from(capacity).unwrap_or(u32::MAX);
            let times = self
                .shared
                .store
                .read(move |q| q.query().recent_fires(&name, since, limit))
                .await?;
            let row = rows.iter().find(|s| s.rule == r.name);
            self.state.restore_rule(
                &r.name,
                RuleRuntime {
                    snoozed_until: row.and_then(|s| s.snoozed_until),
                    circuit_open_until: row.and_then(|s| s.circuit_open_until),
                    fires: FireHistory::from_times(times, capacity),
                },
            );
        }
        Ok(())
    }

    async fn refresh_backlog(&mut self) -> ServiceResult<()> {
        let backlog = self.shared.store.read(|q| q.query().agent_backlog()).await?;
        for agent in &self.snapshot.config.agents {
            let name = &agent.config.name;
            self.state.set_agent_backlog(name, backlog.get(name).copied().unwrap_or(0));
        }
        Ok(())
    }

    pub(crate) async fn run(
        mut self,
        mut control: mpsc::Receiver<Control>,
        mut input: mpsc::Receiver<Input>,
    ) {
        let shutdown = self.shared.shutdown.clone();
        loop {
            let deadline = self.state.next_deadline().map(|d| self.shared.clock.instant_at(d));
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                Some(msg) = control.recv() => self.control(msg).await,
                Some(first) = input.recv() => {
                    let mut batch = vec![first];
                    while batch.len() < BATCH {
                        match input.try_recv() {
                            Ok(more) => batch.push(more),
                            Err(_) => break,
                        }
                    }
                    self.process(batch).await;
                }
                _ = sleep_until(deadline) => self.debounces_due().await,
            }
        }
        // Answer whoever is still waiting.
        input.close();
        while let Ok(msg) = input.try_recv() {
            match msg {
                Input::Source(_) => {}
                Input::Ingest { reply, .. } => {
                    let _ = reply.send(Err(ServiceError::shutting_down()));
                }
                Input::Replay { reply, .. } => {
                    let _ = reply.send(Err(ServiceError::shutting_down()));
                }
            }
        }
    }

    async fn control(&mut self, msg: Control) {
        match msg {
            Control::Config(snapshot) => {
                self.state.retain_configured(&snapshot.routing);
                let limits = &snapshot.config.limits;
                let resized = limits.dedupe_ttl != self.snapshot.config.limits.dedupe_ttl
                    || limits.dedupe_capacity != self.snapshot.config.limits.dedupe_capacity;
                self.snapshot = snapshot;
                if resized {
                    self.reset_dedupe().await;
                }
                if let Err(e) = self.refresh_backlog().await {
                    tracing::warn!("could not recount agent backlogs: {e}");
                }
            }
            Control::Snooze { rule, until, reply } => {
                match until {
                    Some(until) => self.state.snooze(&rule, until),
                    None => self.state.unsnooze(&rule, self.shared.clock.now()),
                }
                let row = self.rule_row(&rule);
                let result = self.shared.store.write(move |s| s.set_rule_state(&row)).await;
                let _ = reply.send(result.map_err(Into::into));
            }
            Control::Paused(p) => self.paused = p,
            Control::RunStarted(run) => {
                self.recent.retain(|r| r.id != run.id);
                if self.recent.len() >= MAX_RECENT_RUNS {
                    // Forget the run that ended first; active runs are kept.
                    if let Some(i) = self
                        .recent
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.ended_at.is_some())
                        .min_by_key(|(_, r)| r.ended_at)
                        .map(|(i, _)| i)
                    {
                        self.recent.remove(i);
                    }
                }
                self.recent.push(run);
            }
            Control::RunWrites { id, writes } => {
                if let Some(r) = self.recent.iter_mut().find(|r| r.id == id) {
                    r.writes = writes;
                }
            }
            Control::RunEnded { id, at } => {
                if let Some(r) = self.recent.iter_mut().find(|r| r.id == id) {
                    r.ended_at = Some(at);
                }
            }
            Control::RefreshBacklog => {
                if let Err(e) = self.refresh_backlog().await {
                    tracing::warn!("could not recount agent backlogs: {e}");
                }
            }
            Control::CopyState(reply) => {
                let _ = reply.send(self.state.clone());
            }
        }
    }

    fn rule_row(&self, rule: &str) -> RuleStateRow {
        let rt = self.state.rule(rule);
        RuleStateRow {
            rule: rule.to_owned(),
            snoozed_until: rt.and_then(|r| r.snoozed_until),
            circuit_open_until: rt.and_then(|r| r.circuit_open_until),
            last_fired_at: rt.and_then(|r| r.fires.last()),
        }
    }

    fn forget_ended_runs(&mut self, now: Timestamp) {
        let grace = self.snapshot.config.limits.causation_grace;
        self.recent.retain(|r| r.ended_at.is_none_or(|end| end.saturating_add(grace) >= now));
    }

    /// The run with this id, for exact causation and the loop guard: from memory, else from
    /// the store (a run that ended a while ago can still report through `pulse run`).
    async fn known_run(&self, id: RunId) -> Option<RecentRun> {
        if let Some(r) = self.recent.iter().find(|r| r.id == id) {
            return Some(r.clone());
        }
        let run = self.shared.store.read(move |q| q.query().run(id)).await.ok().flatten()?;
        Some(RecentRun {
            id: run.id,
            rule: run.rule,
            // Only used as an exact cause: no inference.
            cwds: Vec::new(),
            started_at: run.started_at.unwrap_or(run.created_at),
            ended_at: run.ended_at,
            chain_depth: run.chain_depth,
            // Only used as an exact cause (no working directory, so never inferred).
            writes: RunWrites::opaque(Inference::Reloaded),
        })
    }

    async fn process(&mut self, batch: Vec<Input>) {
        let now = self.shared.clock.now();
        self.forget_ended_runs(now);
        let before = self.state.clone();
        let mut p = Pending { commit: Commit { now, ..Commit::default() }, ..Pending::default() };
        for input in batch {
            match input {
                Input::Source(new) => {
                    self.accept(new, now, &mut p).await;
                }
                Input::Ingest { events, reply } => {
                    let mut ids = Vec::with_capacity(events.len());
                    for new in events {
                        ids.push(self.accept(new, now, &mut p).await.map(|e| e.id));
                    }
                    p.replies.push(Reply::Ingest(reply, ids));
                }
                Input::Replay { event, replay_of, rules, reply } => {
                    let event = event.into_event(self.shared.ids.event(), None, Some(replay_of));
                    let mode = match &rules {
                        Some(names) => RouteMode::Only(names),
                        None => RouteMode::AllRules,
                    };
                    let first = p.commit.deliveries.len();
                    self.route(&event, mode, None, now, &mut p);
                    let deliveries = p.commit.deliveries[first..].to_vec();
                    p.commit.events.push(event.clone());
                    p.replies.push(Reply::Replay(reply, Box::new(event), deliveries));
                }
            }
        }
        self.commit(p, before).await;
    }

    /// Dedupe, attribution, id, routing. `None` if the event was a duplicate.
    async fn accept(&mut self, new: NewEvent, now: Timestamp, p: &mut Pending) -> Option<Event> {
        if let Some(key) = &new.dedupe_key {
            // Keys are per source: two sources may use the same key for different things.
            if !self.dedupe.check(&format!("{}\0{key}", new.source.uri()), now) {
                return None;
            }
        }
        let exact = match new.causation_run {
            Some(id) => self.known_run(id).await,
            None => None,
        };
        let grace = self.snapshot.config.limits.causation_grace;
        let attribution = attribute(&new, exact.as_ref(), &self.recent, grace);
        let cause_rule = match attribution {
            Some(a) if exact.as_ref().is_some_and(|r| r.id == a.run) => {
                exact.as_ref().and_then(|r| r.rule.clone())
            }
            Some(a) => self.recent.iter().find(|r| r.id == a.run).and_then(|r| r.rule.clone()),
            None => None,
        };
        let event = new.into_event(self.shared.ids.event(), attribution, None);
        let cause = attribution.map(|a| (a.run, cause_rule));
        self.route(&event, RouteMode::AllRules, cause, now, p);
        p.commit.events.push(event.clone());
        Some(event)
    }

    fn route(
        &mut self,
        event: &Event,
        mode: RouteMode<'_>,
        cause: Option<(RunId, Option<String>)>,
        now: Timestamp,
        p: &mut Pending,
    ) {
        let cause = cause.as_ref().map(|(run, rule)| Cause { run: *run, rule: rule.as_deref() });
        let mut ctx = RouteContext { now, paused: self.paused, cause, nonces: &mut self.nonces };
        let decisions = route(event, mode, &self.snapshot.routing, &mut self.state, &mut ctx);
        for d in decisions {
            let created = match &d.outcome {
                Outcome::Fire { prompt } => Some((RunStatus::Queued, prompt)),
                Outcome::AwaitApproval { prompt } => Some((RunStatus::PendingApproval, prompt)),
                Outcome::Debounced { .. } | Outcome::Waiting { .. } | Outcome::Blocked { .. } => {
                    None
                }
            };
            let run = created.map(|(status, prompt)| {
                let id = self.shared.ids.run();
                let snapshot = snapshot_of(&self.snapshot.config, &d.rule, &d.agent);
                // Rendered now so an approval shows what will run; the worker renders it again
                // before starting and refuses a difference.
                let parts = release::RunParts {
                    id,
                    rule: Some(&d.rule),
                    agent_name: &d.agent,
                    agent: &snapshot.agent,
                    prompt: prompt.as_ref().map(|p| p.text.as_str()),
                    chain_depth: d.chain_depth,
                };
                let invocation =
                    release::render(&self.shared, &self.snapshot.config, &parts, event)
                        .ok()
                        .map(|i| i.summary());
                let new = NewRun {
                    id,
                    rule: Some(d.rule.clone()),
                    agent: d.agent.clone(),
                    event: event.id,
                    status,
                    created_at: now,
                    prompt: prompt.as_ref().map(|p| p.text.clone()),
                    chain_depth: d.chain_depth,
                    rerun_of: None,
                    snapshot,
                    invocation,
                };
                if status == RunStatus::Queued {
                    p.commit.outbox.push(Effect::StartRun { run: id });
                }
                p.runs.push(summary(&new));
                p.commit.runs.push(new);
                id
            });
            // A breaker that trips now holds the rule until exactly `now + open_for`; one that was
            // already open reports an earlier deadline. A false positive only costs clients a
            // refetch.
            if let Outcome::Blocked { reason: BlockReason::CircuitOpen { until } } = &d.outcome {
                let open_for = self.snapshot.config.limits.circuit_breaker.open_for;
                p.breaker_tripped |= *until == now.saturating_add(open_for);
            }
            p.rules_touched.insert(d.rule.clone());
            p.commit.deliveries.push(Delivery {
                id: self.shared.ids.delivery(),
                event: event.id,
                rule: d.rule,
                agent: d.agent,
                at: now,
                outcome: d.outcome.delivery(),
                run,
            });
        }
    }

    async fn debounces_due(&mut self) {
        let now = self.shared.clock.now();
        let due = self.state.take_due(now);
        // Taken: if the commit fails, these debounces are lost rather than retried at once
        // against a store that is failing.
        let before = self.state.clone();
        let mut p = Pending { commit: Commit { now, ..Commit::default() }, ..Pending::default() };
        for d in due {
            let id = d.event;
            let event = match self.shared.store.read(move |q| q.query().event(id)).await {
                Ok(Some(e)) => e,
                // Pruned or unreadable: the debounce has nothing to fire with.
                Ok(None) => continue,
                Err(e) => {
                    tracing::error!("debounced event {id}: {e}");
                    continue;
                }
            };
            let cause = match event.causation_run {
                Some(run) => Some((run, self.known_run(run).await.and_then(|r| r.rule))),
                None => None,
            };
            self.route(&event, RouteMode::DebounceElapsed(&d.rule), cause, now, &mut p);
            p.earlier.push(event);
        }
        self.commit(p, before).await;
    }

    /// `before` is the routing state as it was before this batch was routed.
    async fn commit(&mut self, mut p: Pending, before: RouterState) {
        if p.commit.events.is_empty() && p.commit.deliveries.is_empty() {
            answer(p.replies);
            return;
        }
        for rule in &p.rules_touched {
            p.commit.rule_states.push(self.rule_row(rule));
        }
        let wake_worker = !p.commit.outbox.is_empty();
        let commit = std::mem::take(&mut p.commit);
        let events = commit.events.clone();
        let deliveries = commit.deliveries.clone();
        match self.shared.store.write(move |s| s.commit(&commit)).await {
            Ok(()) => {
                answer(p.replies);
                self.shared.hub.publish_events(events, &deliveries, &p.runs, p.earlier);
                self.shared.publish_runs(p.runs);
                if p.breaker_tripped {
                    self.shared.notify_state(&[StateTopic::Rules]);
                }
                if wake_worker {
                    self.shared.worker.wake();
                }
            }
            Err(e) => {
                tracing::error!("could not commit {} events: {e}", events.len());
                let err = ServiceError::from(e);
                for r in p.replies {
                    let msg = ServiceError::Internal(err.to_string());
                    match r {
                        Reply::Ingest(tx, _) => drop(tx.send(Err(msg))),
                        Reply::Replay(tx, _, _) => drop(tx.send(Err(msg))),
                    }
                }
                // Routing counted fires, armed debounces, tripped breakers and raised backlogs
                // that were not stored: go back to the state that matches the log. (Rebuilding
                // it from the log would also drop debounces pending from earlier batches.)
                self.state = before;
                // `accept` marked the batch's keys as seen; the sender's retry is not a duplicate.
                self.reset_dedupe().await;
            }
        }
    }
}

fn answer(replies: Vec<Reply>) {
    for r in replies {
        match r {
            Reply::Ingest(tx, ids) => drop(tx.send(Ok(ids))),
            Reply::Replay(tx, event, deliveries) => drop(tx.send(Ok((*event, deliveries)))),
        }
    }
}

/// What a run executes, as configured now.
#[allow(clippy::expect_used)]
pub(crate) fn snapshot_of(config: &Config, rule: &str, agent: &str) -> RunSnapshot {
    RunSnapshot {
        rule: config.rule(rule).map(|r| r.config.clone()),
        // Routing only fires a rule whose agent it compiled from this same config, and config
        // resolution rejects rules naming a missing agent.
        agent: config.agent(agent).map(|a| a.config.clone()).expect("a fired rule's agent exists"),
    }
}

pub(crate) fn summary(r: &NewRun) -> RunSummary {
    RunSummary {
        id: r.id,
        rule: r.rule.clone(),
        agent: r.agent.clone(),
        event: r.event,
        status: r.status,
        created_at: r.created_at,
        started_at: None,
        ended_at: None,
        exit_code: None,
        chain_depth: r.chain_depth,
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
