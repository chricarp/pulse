//! The effect worker: performs what the engine committed to the outbox.
//!
//! It claims `start_run` effects, keeps claimed runs in a FIFO per agent, and starts them while
//! the agent is under `max_concurrent` (and the daemon is not paused). Each started run is a
//! task that builds the invocation through the agent's adapter, runs it under the supervisor,
//! appends output to the store as it comes (a slow store slows the process's pipe, never
//! buffers) and records how the run ended.
//!
//! Claimed rows stay `claimed` until their run starts; after a crash the store returns them to
//! pending. A run is re-read before it starts, so one cancelled while it waited is skipped.
//! On shutdown every active run is cancelled (the supervisor terminates its process group) and
//! the worker waits for all of them to be reaped and recorded as `interrupted`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pulse_agents::{RunEnd, RunRequest, SuperviseError, SuperviseOptions, SupervisorEvent};
use pulse_contracts::pattern::expand_path;
use pulse_contracts::run::InvocationSummary;
use pulse_contracts::{RenderedPrompt, Run, RunId, RunOutput, RunStatus, Timestamp};
use pulse_core::RecentRun;
use pulse_store::{Effect, OutboxItem, RunTransition, StoreError};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::engine::Control;
use crate::shared::Shared;

const CLAIM_BATCH: u32 = 64;
const OUTPUT_CHANNEL: usize = 256;
/// Output chunks appended in one store write.
const OUTPUT_BATCH: usize = 256;
const RETRY_AFTER: pulse_contracts::Duration = pulse_contracts::Duration::from_secs(5);

#[derive(Debug)]
pub(crate) enum WorkerCmd {
    /// Signals an active run. Replies `false` when the run is not active here.
    Cancel { run: RunId, reply: oneshot::Sender<bool> },
}

#[derive(Clone, Debug)]
pub(crate) struct WorkerHandle {
    notify: Arc<Notify>,
    cmd: mpsc::Sender<WorkerCmd>,
}

impl WorkerHandle {
    pub(crate) fn channels() -> (WorkerHandle, mpsc::Receiver<WorkerCmd>) {
        let (tx, rx) = mpsc::channel(64);
        (WorkerHandle { notify: Arc::new(Notify::new()), cmd: tx }, rx)
    }

    /// New outbox rows, a freed slot, a resume or a config change.
    pub(crate) fn wake(&self) {
        self.notify.notify_one();
    }

    pub(crate) fn notify(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    pub(crate) async fn cancel(&self, run: RunId) -> bool {
        let (reply, rx) = oneshot::channel();
        if self.cmd.send(WorkerCmd::Cancel { run, reply }).await.is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }
}

struct Active {
    agent: String,
    cancel: CancellationToken,
    by_user: Arc<AtomicBool>,
}

struct Waiting {
    item: OutboxItem,
    run: Run,
}

pub(crate) struct Worker {
    shared: Arc<Shared>,
    waiting: HashMap<String, VecDeque<Waiting>>,
    active: HashMap<RunId, Active>,
    tasks: JoinSet<RunId>,
    retry_at: Option<Timestamp>,
}

impl Worker {
    pub(crate) fn new(shared: Arc<Shared>) -> Worker {
        Worker {
            shared,
            waiting: HashMap::new(),
            active: HashMap::new(),
            tasks: JoinSet::new(),
            retry_at: None,
        }
    }

    pub(crate) async fn run(mut self, mut cmds: mpsc::Receiver<WorkerCmd>, notify: Arc<Notify>) {
        let shutdown = self.shared.shutdown.clone();
        loop {
            self.claim().await;
            self.start_ready().await;
            self.shared.set_running(self.running_by_agent());
            let retry = self.retry_at.map(|t| self.shared.clock.instant_at(t));
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                Some(cmd) = cmds.recv() => self.command(cmd),
                Some(done) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    if let Ok(run) = done {
                        self.active.remove(&run);
                    }
                }
                _ = notify.notified() => {}
                _ = sleep_until(retry) => self.retry_at = None,
            }
        }
        // Every run's token is a child of the shutdown token, so all are cancelled already.
        while let Some(done) = self.tasks.join_next().await {
            if let Ok(run) = done {
                self.active.remove(&run);
            }
        }
        self.shared.set_running(HashMap::new());
    }

    fn command(&mut self, cmd: WorkerCmd) {
        match cmd {
            WorkerCmd::Cancel { run, reply } => {
                let found = match self.active.get(&run) {
                    Some(a) => {
                        a.by_user.store(true, Ordering::SeqCst);
                        a.cancel.cancel();
                        true
                    }
                    None => false,
                };
                let _ = reply.send(found);
            }
        }
    }

    fn running_by_agent(&self) -> HashMap<String, u32> {
        let mut map = HashMap::new();
        for a in self.active.values() {
            *map.entry(a.agent.clone()).or_insert(0) += 1;
        }
        map
    }

    async fn claim(&mut self) {
        let now = self.shared.clock.now();
        loop {
            let items =
                match self.shared.store.write(move |s| s.claim_outbox(now, CLAIM_BATCH)).await {
                    Ok(items) => items,
                    Err(e) => {
                        tracing::error!("could not claim outbox effects: {e}");
                        self.retry_at = Some(now.saturating_add(RETRY_AFTER));
                        return;
                    }
                };
            let full = items.len() == CLAIM_BATCH as usize;
            for item in items {
                let Effect::StartRun { run: id } = item.effect.clone();
                match self.shared.store.read(move |q| q.query().run(id)).await {
                    Ok(Some(run)) if run.status == RunStatus::Queued => {
                        self.waiting
                            .entry(run.agent.clone())
                            .or_default()
                            .push_back(Waiting { item, run });
                    }
                    Ok(_) => self.complete(item.id).await,
                    Err(e) => {
                        tracing::error!("run {id}: {e}");
                        self.fail(item.id, &e.to_string(), now).await;
                    }
                }
            }
            if !full {
                return;
            }
        }
    }

    async fn complete(&self, id: i64) {
        if let Err(e) = self.shared.store.write(move |s| s.complete_outbox(id)).await {
            tracing::warn!("outbox item {id}: {e}");
        }
    }

    async fn fail(&mut self, id: i64, error: &str, now: Timestamp) {
        let retry = now.saturating_add(RETRY_AFTER);
        let error = error.to_owned();
        let _ = self.shared.store.write(move |s| s.fail_outbox(id, &error, Some(retry))).await;
        self.retry_at = Some(self.retry_at.map_or(retry, |r| r.min(retry)));
    }

    fn max_concurrent(&self, agent: &str, run: &Run) -> u32 {
        let config = self.shared.snapshot();
        config
            .config
            .agent(agent)
            .map_or(run.snapshot.agent.max_concurrent, |a| a.config.max_concurrent)
    }

    async fn start_ready(&mut self) {
        if self.shared.paused() || self.shared.shutdown.is_cancelled() {
            return;
        }
        let agents: Vec<String> = self.waiting.keys().cloned().collect();
        for agent in agents {
            loop {
                let running = self.active.values().filter(|a| a.agent == agent).count() as u32;
                let Some(front) = self.waiting.get(&agent).and_then(|q| q.front()) else { break };
                if running >= self.max_concurrent(&agent, &front.run) {
                    break;
                }
                let Some(w) = self.waiting.get_mut(&agent).and_then(VecDeque::pop_front) else {
                    break;
                };
                let id = w.run.id;
                // It may have been cancelled while it waited.
                match self.shared.store.read(move |q| q.query().run(id)).await {
                    Ok(Some(run)) if run.status == RunStatus::Queued => {
                        let cancel = self.shared.shutdown.child_token();
                        let by_user = Arc::new(AtomicBool::new(false));
                        self.active.insert(
                            id,
                            Active {
                                agent: agent.clone(),
                                cancel: cancel.clone(),
                                by_user: by_user.clone(),
                            },
                        );
                        let task = RunTask {
                            shared: self.shared.clone(),
                            item: w.item,
                            run,
                            cancel,
                            by_user,
                        };
                        self.tasks.spawn(task.run());
                    }
                    _ => self.complete(w.item.id).await,
                }
            }
            if self.waiting.get(&agent).is_some_and(VecDeque::is_empty) {
                self.waiting.remove(&agent);
            }
        }
    }
}

/// One run, from invocation to recorded end.
struct RunTask {
    shared: Arc<Shared>,
    item: OutboxItem,
    run: Run,
    cancel: CancellationToken,
    by_user: Arc<AtomicBool>,
}

enum Prepared {
    Ready { invocation: Box<pulse_agents::Invocation>, parser: Box<dyn pulse_agents::OutputParser> },
    Failed(String),
}

impl RunTask {
    async fn run(self) -> RunId {
        let id = self.run.id;
        match self.prepare().await {
            Prepared::Failed(error) => self.fail_to_start(error).await,
            Prepared::Ready { invocation, parser } => {
                self.supervise(*invocation, parser).await;
            }
        }
        id
    }

    async fn prepare(&self) -> Prepared {
        let run = &self.run;
        let agent = &run.snapshot.agent;
        let Some(adapter) = (self.shared.adapters)(agent.kind()) else {
            return Prepared::Failed(format!(
                "agent {:?} uses an adapter Pulse does not run",
                agent.name
            ));
        };
        let event_id = run.event;
        let event = match self.shared.store.read(move |q| q.query().event(event_id)).await {
            Ok(Some(e)) => e,
            Ok(None) => {
                return Prepared::Failed(format!("event {event_id} is no longer in the log"));
            }
            Err(e) => return Prepared::Failed(e.to_string()),
        };
        let dir = self.shared.home.run_dir(run.id);
        let event_file = dir.join("event.json");
        let written = tokio::fs::create_dir_all(&dir).await.and(
            tokio::fs::write(&event_file, serde_json::to_vec_pretty(&event).unwrap_or_default())
                .await,
        );
        if let Err(e) = written {
            return Prepared::Failed(format!("cannot write {}: {e}", event_file.display()));
        }
        let snapshot = self.shared.snapshot();
        let base =
            snapshot.config.agent(&run.agent).and_then(|a| a.origin.base_dir().map(PathBuf::from));
        let cwd = match &agent.cwd {
            Some(raw) => match expand_path(raw, &self.shared.user_home, base.as_deref()) {
                Ok(p) => PathBuf::from(p),
                Err(e) => return Prepared::Failed(format!("agent cwd: {e}")),
            },
            None => dir.clone(),
        };
        let request = RunRequest {
            run_id: run.id,
            rule: run.rule.clone(),
            agent: run.agent.clone(),
            event,
            // Only the text is stored; adapters send the text.
            prompt: run.prompt.clone().map(|text| RenderedPrompt {
                text,
                delimiter: String::new(),
                truncated: false,
                missing: Vec::new(),
            }),
            chain_depth: run.chain_depth,
            cwd,
            event_file,
            pulse_home: self.shared.home.root().to_path_buf(),
            limits: snapshot.config.limits.prompt.clone(),
        };
        match adapter.invocation(agent, &request) {
            Ok(invocation) => {
                Prepared::Ready { invocation: Box::new(invocation), parser: adapter.parser() }
            }
            Err(e) => Prepared::Failed(e.to_string()),
        }
    }

    async fn transition(&self, t: RunTransition) -> Result<Run, StoreError> {
        self.transition_at(t, self.shared.clock.now()).await
    }

    async fn transition_at(&self, t: RunTransition, now: Timestamp) -> Result<Run, StoreError> {
        let id = self.run.id;
        let run = self.shared.store.write(move |s| s.transition_run(id, t, now)).await?;
        self.shared.publish_runs(vec![run.summary()]);
        Ok(run)
    }

    async fn complete_item(&self) {
        let id = self.item.id;
        if let Err(e) = self.shared.store.write(move |s| s.complete_outbox(id)).await {
            tracing::warn!("outbox item {id}: {e}");
        }
    }

    async fn fail_to_start(&self, error: String) {
        tracing::warn!("run {} could not start: {error}", self.run.id);
        if let Err(e) = self.transition(RunTransition::FailToStart { error }).await {
            tracing::warn!("run {}: {e}", self.run.id);
        }
        self.complete_item().await;
        let _ = self.shared.engine.control(Control::RefreshBacklog).await;
    }

    /// `queued → running`, as of `at`. `false` if the run is no longer queued (cancelled
    /// meanwhile).
    async fn start(&self, pid: Option<u32>, invocation: &InvocationSummary, at: Timestamp) -> bool {
        let t = RunTransition::Start { pid, invocation: invocation.clone() };
        let started = self.transition_at(t, at).await;
        self.complete_item().await;
        match started {
            Ok(run) => {
                let recent = RecentRun {
                    id: run.id,
                    rule: run.rule.clone(),
                    cwd: Some(invocation.cwd.clone()),
                    started_at: run.started_at.unwrap_or(run.created_at),
                    ended_at: None,
                    chain_depth: run.chain_depth,
                };
                let _ = self.shared.engine.control(Control::RunStarted(recent)).await;
                true
            }
            Err(e) => {
                tracing::info!("run {} was not started: {e}", self.run.id);
                false
            }
        }
    }

    async fn append(&self, chunks: Vec<(Timestamp, RunOutput)>) {
        let id = self.run.id;
        let cap = self.shared.snapshot().config.limits.run_output_max_bytes;
        match self.shared.store.write(move |s| s.append_output(id, &chunks, cap)).await {
            Ok(_) => self.shared.hub.output_appended(id),
            Err(e) => tracing::warn!("run {id}: could not store output: {e}"),
        }
    }

    async fn supervise(
        &self,
        invocation: pulse_agents::Invocation,
        parser: Box<dyn pulse_agents::OutputParser>,
    ) {
        let summary = invocation.summary();
        // The run starts before its process does: inferred causation compares event times with
        // this, and an agent can write a file before the worker gets to record the start.
        let spawned_at = self.shared.clock.now();
        let options = SuperviseOptions {
            timeout: self.run.snapshot.agent.timeout.to_std(),
            ..SuperviseOptions::default()
        };
        let (tx, mut rx) = mpsc::channel(OUTPUT_CHANNEL);
        let mut supervised =
            self.shared.supervisor.supervise(invocation, parser, tx, options, self.cancel.clone());
        // None until the run is recorded as running; Some(false) if it turned out cancelled.
        let mut started: Option<bool> = None;
        let mut result = None;
        loop {
            let event = tokio::select! {
                ev = rx.recv() => ev,
                res = &mut supervised, if result.is_none() => {
                    result = Some(res);
                    continue;
                }
            };
            let Some(event) = event else { break };
            match event {
                SupervisorEvent::Started { pid } => {
                    if started.is_none() {
                        started = Some(self.start(Some(pid), &summary, spawned_at).await);
                        if started == Some(false) {
                            self.cancel.cancel();
                        }
                    }
                }
                SupervisorEvent::Output { at, output } => {
                    if started.is_none() {
                        started = Some(self.start(None, &summary, spawned_at).await);
                    }
                    let mut chunks = vec![(at, output)];
                    while chunks.len() < OUTPUT_BATCH {
                        match rx.try_recv() {
                            Ok(SupervisorEvent::Output { at, output }) => chunks.push((at, output)),
                            Ok(SupervisorEvent::Started { .. }) => {}
                            Err(_) => break,
                        }
                    }
                    if started == Some(true) {
                        self.append(chunks).await;
                    }
                }
            }
        }
        // The channel closed, so the supervisor has returned or is about to.
        let result = match result {
            Some(r) => r,
            None => supervised.await,
        };
        self.finish(started, result).await;
    }

    async fn finish(&self, started: Option<bool>, result: Result<RunEnd, SuperviseError>) {
        let started = match (started, &result) {
            (Some(s), _) => s,
            (None, Err(e)) => {
                return self.fail_to_start(e.to_string()).await;
            }
            (None, Ok(_)) => {
                self.start(
                    None,
                    &InvocationSummary {
                        program: String::new(),
                        args: Vec::new(),
                        cwd: String::new(),
                    },
                    self.shared.clock.now(),
                )
                .await
            }
        };
        if !started {
            return;
        }
        let shutting_down =
            self.shared.shutdown.is_cancelled() && !self.by_user.load(Ordering::SeqCst);
        let (status, exit_code, signal, error) = match result {
            Ok(RunEnd::Exited { code: Some(0), signal }) => {
                (RunStatus::Succeeded, Some(0), signal, None)
            }
            Ok(RunEnd::Exited { code, signal }) => (RunStatus::Failed, code, signal, None),
            Ok(RunEnd::TimedOut) => (RunStatus::TimedOut, None, None, None),
            Ok(RunEnd::Cancelled) if shutting_down => {
                (RunStatus::Interrupted, None, None, Some("the daemon shut down".to_owned()))
            }
            Ok(RunEnd::Cancelled) => (RunStatus::Cancelled, None, None, None),
            Err(e) => (RunStatus::Failed, None, None, Some(e.to_string())),
        };
        let t = RunTransition::Finish { status, exit_code, signal, error };
        match self.transition(t).await {
            Ok(run) => {
                let at = run.ended_at.unwrap_or_else(|| self.shared.clock.now());
                let _ = self.shared.engine.control(Control::RunEnded { id: run.id, at }).await;
            }
            Err(e) => tracing::error!("run {}: could not record its end: {e}", self.run.id),
        }
        let _ = self.shared.engine.control(Control::RefreshBacklog).await;
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
