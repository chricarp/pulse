//! `runs.*`: listing, output (stored and live), approvals, cancel and rerun.

use std::time::Duration;

use pulse_contracts::rpc::{
    RunFilter, RunOutputPage, RunPage, RunsChanged, RunsOutput, SHORT_ID_WINDOW,
};
use pulse_contracts::run::RunSnapshot;
use pulse_contracts::{Run, RunId, RunStatus, RunSummary, SubscriptionId};
use pulse_store::{Commit, Effect, NewRun, RunTransition};
use tokio_util::sync::CancellationToken;

use super::Services;
use crate::engine::{Control, summary};
use crate::error::{ServiceError, ServiceResult};
use crate::hub::{STUCK_LIMIT, Sent};
use crate::release;

/// Chunks per `runs.output` notification.
const OUTPUT_PAGE: u32 = 500;
/// At most this often per output subscription, however chatty the agent.
const OUTPUT_EVERY: Duration = Duration::from_millis(100);

impl Services {
    pub async fn runs_list(
        &self,
        filter: RunFilter,
        cursor: Option<RunId>,
        limit: Option<u32>,
    ) -> ServiceResult<RunPage> {
        Ok(self.shared.store.read(move |q| q.query().list_runs(&filter, cursor, limit)).await?)
    }

    /// The full id of the run whose id ends in `id` (see `ResolveIdParams`).
    pub async fn runs_resolve_id(&self, id: &str) -> ServiceResult<RunId> {
        if let Ok(full) = id.parse() {
            return Ok(full);
        }
        let suffix = super::id_suffix(id, "run")?;
        let (s, max) = (suffix.clone(), super::SHORT_ID_CANDIDATES as u32 + 1);
        let found = self
            .shared
            .store
            .read(move |q| q.query().run_ids_ending_in(&s, SHORT_ID_WINDOW, max))
            .await?;
        super::one_id(found, &suffix, "run")
    }

    pub async fn runs_get(&self, id: RunId) -> ServiceResult<Run> {
        self.shared
            .store
            .read(move |q| q.query().run(id))
            .await?
            .ok_or_else(|| ServiceError::not_found("run", id))
    }

    pub async fn runs_output(
        &self,
        id: RunId,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> ServiceResult<RunOutputPage> {
        Ok(self.shared.store.read(move |q| q.query().run_output(id, after, limit)).await?)
    }

    /// Run changes, from when `ready` is cancelled (after the subscribe call was answered).
    pub(crate) fn runs_subscribe(
        &self,
        filter: RunFilter,
        deliver: impl FnMut(&RunsChanged) -> Sent + Send + 'static,
        ready: CancellationToken,
        stop: CancellationToken,
        client: CancellationToken,
    ) -> SubscriptionId {
        let id = self.shared.ids.subscription();
        self.shared.hub.spawn_run_subscription(id, filter, deliver, ready, stop, client);
        id
    }

    /// Streams a run's output: stored chunks after `after`, then live ones, then `done`. The
    /// subscription keeps a cursor into the store rather than a queue, so a slow client loses
    /// nothing and costs nothing while it catches up. Nothing is sent before `ready`.
    pub(crate) async fn runs_subscribe_output(
        &self,
        run: RunId,
        after: Option<u64>,
        mut deliver: impl FnMut(&RunsOutput) -> Sent + Send + 'static,
        ready: CancellationToken,
        stop: CancellationToken,
        client: CancellationToken,
    ) -> ServiceResult<SubscriptionId> {
        // Watch first, then read the status: a run that ends in between closes the watch
        // instead of leaving it open forever.
        let mut watch = Some(self.shared.hub.output_watch(run));
        let current = self.runs_get(run).await;
        let terminal = current.as_ref().map_or(true, |r| r.status.is_terminal());
        if terminal {
            watch = None;
            self.shared.hub.forget_output(run);
        }
        current?;
        let subscription = self.shared.ids.subscription();
        let shared = self.shared.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = client.cancelled() => return,
                _ = ready.cancelled() => {}
            }
            let mut cursor = after;
            loop {
                let page = match shared
                    .store
                    .read(move |q| q.query().run_output(run, cursor, Some(OUTPUT_PAGE)))
                    .await
                {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("output of run {run}: {e}");
                        return;
                    }
                };
                let more = page.chunks.len() == OUTPUT_PAGE as usize;
                let done = page.done && !more;
                if !page.chunks.is_empty() || done {
                    let msg = RunsOutput { subscription, run, chunks: page.chunks, done };
                    let mut full_since = None;
                    loop {
                        match deliver(&msg) {
                            Sent::Ok => break,
                            Sent::Closed => return,
                            Sent::Full => {
                                let since =
                                    *full_since.get_or_insert_with(tokio::time::Instant::now);
                                if since.elapsed() >= STUCK_LIMIT {
                                    client.cancel();
                                    return;
                                }
                                tokio::select! {
                                    _ = stop.cancelled() => return,
                                    _ = client.cancelled() => return,
                                    _ = tokio::time::sleep(OUTPUT_EVERY) => {}
                                }
                            }
                        }
                    }
                    cursor = page.next_after;
                    if done {
                        return;
                    }
                }
                if more {
                    continue;
                }
                match &mut watch {
                    Some(w) => {
                        tokio::select! {
                            _ = stop.cancelled() => return,
                            _ = client.cancelled() => return,
                            changed = w.changed() => {
                                // Closed: the run ended; the next read says `done`.
                                if changed.is_err() {
                                    watch = None;
                                }
                            }
                        }
                        tokio::time::sleep(OUTPUT_EVERY).await;
                    }
                    // Not done, but the run has ended per the hub: read until the store agrees.
                    None => tokio::time::sleep(OUTPUT_EVERY).await,
                }
            }
        });
        Ok(subscription)
    }

    async fn transition(&self, id: RunId, t: RunTransition) -> ServiceResult<Run> {
        let now = self.shared.clock.now();
        let run = self.shared.store.write(move |s| s.transition_run(id, t, now)).await?;
        self.shared.publish_runs(vec![run.summary()]);
        let _ = self.shared.engine.control(Control::RefreshBacklog).await;
        Ok(run)
    }

    /// Approves a pending run, unless its agent or rule changed since it was created (what the
    /// approval showed would not be what runs): then the run is withdrawn and the call fails
    /// with the reason.
    pub async fn runs_approve(&self, id: RunId) -> ServiceResult<RunSummary> {
        let pending = self.runs_get(id).await?;
        if pending.status == RunStatus::PendingApproval {
            let config = self.shared.snapshot();
            if let Some(reason) =
                release::stale_reason(&config.config, &pending.agent, &pending.snapshot)
            {
                self.transition(id, RunTransition::Withdraw { reason: reason.clone() }).await?;
                return Err(ServiceError::Conflict(format!("not approved: {reason}")));
            }
        }
        let run = self.transition(id, RunTransition::Approve).await?;
        self.shared.worker.wake();
        Ok(run.summary())
    }

    pub async fn runs_reject(
        &self,
        id: RunId,
        reason: Option<String>,
    ) -> ServiceResult<RunSummary> {
        Ok(self.transition(id, RunTransition::Reject { reason }).await?.summary())
    }

    /// A running run is signalled (its process group is terminated) and becomes `cancelled`
    /// when the supervisor has reaped it; a waiting run is cancelled at once.
    pub async fn runs_cancel(&self, id: RunId) -> ServiceResult<RunSummary> {
        if self.shared.worker.cancel(id).await {
            return Ok(self.runs_get(id).await?.summary());
        }
        Ok(self.transition(id, RunTransition::Cancel).await?.summary())
    }

    /// A new queued run with the same event and prompt, executed with the agent (and rule) as
    /// they are now: an old snapshot is never resurrected. It is the user's explicit request,
    /// so it skips routing (no approval, cooldown or rate limit). Refused if the agent no
    /// longer exists.
    pub async fn runs_rerun(&self, id: RunId) -> ServiceResult<RunSummary> {
        let old = self.runs_get(id).await?;
        let event_id = old.event;
        let Some(event) = self.shared.store.read(move |q| q.query().event(event_id)).await? else {
            return Err(ServiceError::Conflict(format!(
                "run {id}'s event {event_id} has been pruned from the log"
            )));
        };
        let config = self.shared.snapshot();
        let agent = config.config.agent(&old.agent).ok_or_else(|| {
            ServiceError::Conflict(format!(
                "the agent {:?} no longer exists, so this run cannot be run again",
                old.agent
            ))
        })?;
        let rule = old.rule.as_deref().and_then(|r| config.config.rule(r));
        let snapshot =
            RunSnapshot { rule: rule.map(|r| r.config.clone()), agent: agent.config.clone() };
        let new_id = self.shared.ids.run();
        let parts = release::RunParts {
            id: new_id,
            rule: rule.map(|r| r.config.name.as_str()),
            agent_name: &old.agent,
            agent: &snapshot.agent,
            prompt: old.prompt.as_deref(),
            chain_depth: old.chain_depth,
        };
        let invocation =
            release::render(&self.shared, &config.config, &parts, &event).ok().map(|i| i.summary());
        let now = self.shared.clock.now();
        let new = NewRun {
            id: new_id,
            rule: rule.map(|r| r.config.name.clone()),
            agent: old.agent,
            event: event_id,
            status: RunStatus::Queued,
            created_at: now,
            prompt: old.prompt,
            chain_depth: old.chain_depth,
            rerun_of: Some(id),
            snapshot,
            invocation,
        };
        let created = summary(&new);
        let commit = Commit {
            now,
            outbox: vec![Effect::StartRun { run: new.id }],
            runs: vec![new],
            ..Commit::default()
        };
        self.shared.store.write(move |s| s.commit(&commit)).await?;
        self.shared.publish_runs(vec![created.clone()]);
        let _ = self.shared.engine.control(Control::RefreshBacklog).await;
        self.shared.worker.wake();
        Ok(created)
    }
}
