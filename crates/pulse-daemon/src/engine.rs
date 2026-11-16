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
//! Control messages (config, snooze, pause, run starts and ends) have their own channel and are
//! handled before events, so attribution sees a run as soon as it starts.

use std::collections::BTreeSet;
use std::sync::Arc;

use pulse_contracts::config::Config;
use pulse_contracts::run::RunSnapshot;
use pulse_contracts::{
    Delivery, Event, EventId, NewEvent, Outcome, RunId, RunStatus, RunSummary, Timestamp,
};
use pulse_core::dedupe::Dedupe;
use pulse_core::limits::{FireHistory, history_capacity};
use pulse_core::{
    Cause, NonceSource, RecentRun, RouteContext, RouteMode, RouterState, RuleRuntime, attribute,
    route,
};
use pulse_store::{Commit, Effect, NewRun, RuleStateRow};
use tokio::sync::{mpsc, oneshot};

use crate::config::Snapshot;
use crate::error::{ServiceError, ServiceResult};
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
    rules_touched: BTreeSet<String>,
    runs: Vec<RunSummary>,
    replies: Vec<Reply>,
}

enum Reply {
    Ingest(oneshot::Sender<ServiceResult<Vec<Option<EventId>>>>, Vec<Option<EventId>>),
    Replay(oneshot::Sender<ServiceResult<(Event, Vec<Delivery>)>>, Box<Event>, Vec<Delivery>),
}

impl Engine {
    /// Builds the engine with router state rebuilt from the store: snooze and breaker from
    /// `rule_state`, recent fires from deliveries, backlogs from runs, the pause flag.
    pub(crate) async fn restore(
        shared: Arc<Shared>,
        snapshot: Arc<Snapshot>,
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
        Ok(engine)
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
                if limits.dedupe_ttl != self.snapshot.config.limits.dedupe_ttl
                    || limits.dedupe_capacity != self.snapshot.config.limits.dedupe_capacity
                {
                    self.dedupe = Dedupe::new(limits.dedupe_ttl, limits.dedupe_capacity as usize);
                }
                self.snapshot = snapshot;
                if let Err(e) = self.refresh_backlog().await {
                    tracing::warn!("could not recount agent backlogs: {e}");
                }
            }
            Control::Snooze { rule, until, reply } => {
                match until {
                    Some(until) => self.state.snooze(&rule, until),
                    None => self.state.unsnooze(&rule),
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
            cwd: run.invocation.map(|i| i.cwd),
            started_at: run.started_at.unwrap_or(run.created_at),
            ended_at: run.ended_at,
            chain_depth: run.chain_depth,
        })
    }

    async fn process(&mut self, batch: Vec<Input>) {
        let now = self.shared.clock.now();
        self.forget_ended_runs(now);
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
        self.commit(p).await;
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
                Outcome::Debounced { .. } | Outcome::Blocked { .. } => None,
            };
            let run = created.map(|(status, prompt)| {
                let id = self.shared.ids.run();
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
                    snapshot: snapshot_of(&self.snapshot.config, &d.rule, &d.agent),
                };
                if status == RunStatus::Queued {
                    p.commit.outbox.push(Effect::StartRun { run: id });
                }
                p.runs.push(summary(&new));
                p.commit.runs.push(new);
                id
            });
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
        }
        self.commit(p).await;
    }

    async fn commit(&mut self, mut p: Pending) {
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
        match self.shared.store.write(move |s| s.commit(&commit)).await {
            Ok(()) => {
                answer(p.replies);
                self.shared.hub.publish_events(events);
                self.shared.hub.publish_runs(p.runs);
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
                // Routing already counted fires that were not stored; start over from the log.
                let limits = self.snapshot.config.limits.clone();
                self.state = RouterState::new(&limits);
                if let Err(e) = self.restore_rules().await {
                    tracing::error!("could not rebuild routing state: {e}");
                }
                let _ = self.refresh_backlog().await;
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
