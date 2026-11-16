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
//!
//! What a run's adapter reports about its writes goes to the engine as soon as it is read, and
//! is not held up by a slow output append: the engine needs it before the file event that the
//! write causes, which the fs source holds back for its quiet period. Paths are resolved here
//! (the filesystem is the daemon's business): relative to the run's working directory, and in
//! every spelling the watcher might use (as given, canonical, and canonical spelled under the
//! configured working directory, since Claude Code reports the canonical one).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pulse_agents::{RunEnd, SuperviseError, SuperviseOptions, SupervisorEvent, WriteFact};
use pulse_contracts::run::InvocationSummary;
use pulse_contracts::{OutputKind, Run, RunId, RunOutput, RunStatus, Timestamp};
use pulse_core::path::{is_clean_absolute, rebase, resolve_reported};
use pulse_core::{RecentRun, RunWrites};
use pulse_store::{Effect, OutboxItem, RunTransition, StoreError};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::engine::Control;
use crate::release;
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
    Ready {
        invocation: Box<pulse_agents::Invocation>,
        parser: Box<dyn pulse_agents::OutputParser>,
        writes: Writes,
    },
    Failed(String),
}

/// A run's write knowledge, kept here and copied to the engine whenever it changes.
struct Writes {
    state: RunWrites,
    /// The working directory as configured (how the fs source spells it) and canonical.
    cwd: String,
    cwd_real: Option<String>,
}

impl Writes {
    /// Records facts; returns why the run just became opaque, if it did.
    fn record(&mut self, facts: Vec<(WriteFact, Vec<String>)>) -> Option<String> {
        let mut became = None;
        for (fact, spellings) in facts {
            let (now_opaque, why) = match fact {
                WriteFact::Wrote(raw) => {
                    let why = if spellings.is_empty() {
                        let raw: String = raw.chars().take(200).collect();
                        format!("a write to {raw:?}, a path Pulse cannot resolve")
                    } else {
                        "more files written than Pulse remembers per run".to_owned()
                    };
                    (self.state.record_write(&spellings), why)
                }
                WriteFact::Opaque(why) => (self.state.make_opaque(), why),
            };
            if now_opaque {
                became = Some(why);
            }
        }
        became
    }
}

/// The spellings of a reported path, or none if it cannot be resolved. Blocking.
fn spellings(raw: &str, cwd: &str, cwd_real: Option<&str>) -> Vec<String> {
    let Some(path) = resolve_reported(raw, cwd) else { return Vec::new() };
    let mut out = vec![path.clone()];
    if let Some(real) = canonical(Path::new(&path)) {
        if let Some(back) = cwd_real.and_then(|r| rebase(&real, r, cwd)) {
            out.push(back);
        }
        out.push(real);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// A working directory as configured and canonical, for inference (as `spellings` gives a
/// reported path's).
pub(crate) fn cwd_spellings(cwd: &str, cwd_real: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = [Some(cwd), cwd_real]
        .into_iter()
        .flatten()
        .filter(|c| is_clean_absolute(c))
        .map(str::to_owned)
        .collect();
    out.dedup();
    out
}

/// `path` with its longest existing prefix canonicalized (the file itself usually does not
/// exist yet when its write is reported).
fn canonical(path: &Path) -> Option<String> {
    let mut tail = Vec::new();
    let mut base = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            let mut real = real;
            real.extend(tail.iter().rev());
            return real.to_str().map(str::to_owned);
        }
        tail.push(base.file_name()?);
        base = base.parent()?;
    }
}

impl RunTask {
    async fn run(self) -> RunId {
        let id = self.run.id;
        // A run never starts with an agent definition that is not the current one.
        let config = self.shared.snapshot();
        if let Some(reason) =
            release::stale_reason(&config.config, &self.run.agent, &self.run.snapshot)
        {
            self.withdraw(reason).await;
            return id;
        }
        match self.prepare().await {
            Prepared::Failed(error) => self.fail_to_start(error).await,
            Prepared::Ready { invocation, .. }
                if self
                    .run
                    .invocation
                    .as_ref()
                    .is_some_and(|shown| *shown != invocation.summary()) =>
            {
                self.withdraw(
                    "what this run would execute differs from what was recorded when it was created; replay the event to run it again"
                        .to_owned(),
                )
                .await;
            }
            Prepared::Ready { invocation, parser, writes } => {
                self.supervise(*invocation, parser, writes).await;
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
        // The same rendering as when the run was created (see `release`).
        let parts = release::RunParts {
            id: run.id,
            rule: run.rule.as_deref(),
            agent_name: &run.agent,
            agent,
            prompt: run.prompt.as_deref(),
            chain_depth: run.chain_depth,
        };
        match release::render(&self.shared, &snapshot.config, &parts, &event) {
            Ok(invocation) => Prepared::Ready {
                writes: Writes {
                    state: if adapter.reports_writes() {
                        RunWrites::observed()
                    } else {
                        RunWrites::opaque()
                    },
                    cwd: invocation.cwd.display().to_string(),
                    cwd_real: tokio::fs::canonicalize(&invocation.cwd)
                        .await
                        .ok()
                        .and_then(|p| p.to_str().map(str::to_owned)),
                },
                invocation: Box::new(invocation),
                parser: adapter.parser(),
            },
            Err(e) => Prepared::Failed(e),
        }
    }

    /// Ends a queued run that can no longer start as it was created.
    async fn withdraw(&self, reason: String) {
        tracing::info!("run {} withdrawn: {reason}", self.run.id);
        if let Err(e) = self.transition(RunTransition::Withdraw { reason }).await {
            tracing::warn!("run {}: {e}", self.run.id);
        }
        self.complete_item().await;
        let _ = self.shared.engine.control(Control::RefreshBacklog).await;
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

    /// `queued → running`, as of `at`, recorded before the process is spawned (the pid is set
    /// once known): a crash after the spawn then finds a `running` run, recovered as
    /// `interrupted`, never a queued one it would start a second time. The engine learns of the
    /// run here too, so it counts as active for attribution from just before the spawn. `false`
    /// if the run is no longer queued (cancelled meanwhile).
    async fn start(
        &self,
        invocation: &InvocationSummary,
        at: Timestamp,
        writes: RunWrites,
        cwds: Vec<String>,
    ) -> bool {
        let t = RunTransition::Start { pid: None, invocation: invocation.clone() };
        let started = self.transition_at(t, at).await;
        self.complete_item().await;
        match started {
            Ok(run) => {
                let recent = RecentRun {
                    id: run.id,
                    rule: run.rule.clone(),
                    cwds,
                    started_at: run.started_at.unwrap_or(run.created_at),
                    ended_at: None,
                    chain_depth: run.chain_depth,
                    writes,
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
        mut writes: Writes,
    ) {
        let summary = invocation.summary();
        // The run starts before its process does: inferred causation compares event times with
        // this, and an agent can write a file before the worker gets to record the start.
        let spawned_at = self.shared.clock.now();
        let cwds = cwd_spellings(&writes.cwd, writes.cwd_real.as_deref());
        if !self.start(&summary, spawned_at, writes.state.clone(), cwds).await {
            return;
        }
        let options = SuperviseOptions {
            timeout: self.run.snapshot.agent.timeout.to_std(),
            ..SuperviseOptions::default()
        };
        let (tx, mut rx) = mpsc::channel(OUTPUT_CHANNEL);
        let mut supervised =
            self.shared.supervisor.supervise(invocation, parser, tx, options, self.cancel.clone());
        let mut result = None;
        // Output waiting for the append in flight. Bounded: while it is full, nothing more is
        // received, so the supervisor stops reading the agent's pipes.
        let mut queued: Vec<(Timestamp, RunOutput)> = Vec::new();
        // Said after the output of the line that caused it (the tool call), which follows.
        let mut opaque_marker: Option<(Timestamp, RunOutput)> = None;
        let mut appending: Option<Pin<Box<dyn Future<Output = ()> + Send + '_>>> = None;
        let mut closed = false;
        loop {
            if closed {
                queued.extend(opaque_marker.take());
            }
            if appending.is_none() && !queued.is_empty() {
                appending = Some(Box::pin(self.append(std::mem::take(&mut queued))));
            }
            if closed && appending.is_none() {
                break;
            }
            tokio::select! {
                ev = rx.recv(), if !closed && queued.len() < OUTPUT_BATCH => {
                    let Some(event) = ev else {
                        closed = true;
                        continue;
                    };
                    match event {
                        SupervisorEvent::Started { pid } => {
                            let id = self.run.id;
                            let set = self.shared.store.write(move |s| s.set_run_pid(id, pid)).await;
                            if let Err(e) = set {
                                tracing::warn!("run {id}: could not record its pid: {e}");
                            }
                        }
                        SupervisorEvent::Output { at, output } => {
                            queued.push((at, output));
                            queued.extend(opaque_marker.take());
                        }
                        SupervisorEvent::Writes { at, facts } => {
                            if let Some(marker) = self.report_writes(&mut writes, facts).await {
                                opaque_marker = Some((at, marker));
                            }
                        }
                    }
                }
                () = wait(&mut appending), if appending.is_some() => appending = None,
                res = &mut supervised, if result.is_none() => result = Some(res),
            }
        }
        // The channel closed, so the supervisor has returned or is about to.
        let result = match result {
            Some(r) => r,
            None => supervised.await,
        };
        self.finish(result).await;
    }

    /// Resolves reported paths, tells the engine, and returns a marker for the run's output if
    /// the run just became opaque.
    async fn report_writes(&self, writes: &mut Writes, facts: Vec<WriteFact>) -> Option<RunOutput> {
        let (cwd, cwd_real) = (writes.cwd.clone(), writes.cwd_real.clone());
        let resolve = move || {
            facts
                .into_iter()
                .map(|f| {
                    let s = match &f {
                        WriteFact::Wrote(raw) => spellings(raw, &cwd, cwd_real.as_deref()),
                        WriteFact::Opaque(_) => Vec::new(),
                    };
                    (f, s)
                })
                .collect::<Vec<_>>()
        };
        let resolved = match tokio::task::spawn_blocking(resolve).await {
            Ok(r) => r,
            // Losing a fact must not make the run look cleaner than it is.
            Err(_) => vec![(WriteFact::Opaque("a write Pulse failed to resolve".into()), vec![])],
        };
        let before = writes.state.clone();
        let became_opaque = writes.record(resolved);
        if writes.state != before {
            let control = Control::RunWrites { id: self.run.id, writes: writes.state.clone() };
            let _ = self.shared.engine.control(control).await;
        }
        let why = became_opaque?;
        let grace = self.shared.snapshot().config.limits.causation_grace;
        let why: String = why.chars().take(200).collect();
        Some(RunOutput {
            kind: OutputKind::Marker,
            text: format!(
                "Pulse can no longer tell this run's writes apart ({why}). Until {grace} after it \
                 ends, changes in {} are taken to be its own (inferred), yours included.",
                writes.cwd
            ),
            data: Some(serde_json::json!({ "reason": "writes_opaque", "cause": why })),
        })
    }

    /// Records how a running run ended. A process that could not be spawned is `failed` with
    /// the reason (the run was already recorded as running).
    async fn finish(&self, result: Result<RunEnd, SuperviseError>) {
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

/// Waits for the future in `slot`; never resolves when there is none.
async fn wait(slot: &mut Option<Pin<Box<dyn Future<Output = ()> + Send + '_>>>) {
    match slot {
        Some(f) => f.await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fs source spells paths under the configured folder; Claude Code reports canonical
    /// ones (its working directory is the realpath). A reported write must match either.
    #[test]
    fn reported_paths_in_every_spelling_the_watcher_may_use() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().canonicalize().unwrap().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (real, link) = (real.display().to_string(), link.display().to_string());
        let cwd_real = Some(real.as_str());
        let both = |rel: &str| {
            let mut v = vec![format!("{link}/{rel}"), format!("{real}/{rel}")];
            v.sort();
            v
        };
        let cases = [
            (format!("{real}/a.md"), both("a.md")),
            (format!("{link}/a.md"), both("a.md")),
            ("a.md".to_owned(), both("a.md")),
            // The directory does not exist yet: its existing ancestor is resolved.
            ("notes/new/a.md".to_owned(), both("notes/new/a.md")),
            ("/elsewhere/x".to_owned(), vec!["/elsewhere/x".to_owned()]),
            ("../a.md".to_owned(), vec![]),
        ];
        for (raw, want) in cases {
            assert_eq!(spellings(&raw, &link, cwd_real), want, "{raw}");
        }
    }
}
