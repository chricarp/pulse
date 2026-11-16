//! `rules.*`: definitions (in config files), runtime state (snooze, breaker), dry runs and
//! history.

use pulse_contracts::config::{RuleConfig, Sourced, validate_name};
use pulse_contracts::rpc::{
    DeliveryPage, RuleHistoryParams, RuleInfo, RuleList, RuleRef, RuleRuntimeStatus,
    RuleTestParams, RuleTestResult, RuleUpsertParams,
};
use pulse_contracts::{BlockReason, DeliveryOutcome, Outcome, Timestamp};
use pulse_core::{Cause, RouteContext, RouteMode, RuleRuntime, route};
use pulse_store::RuleStateRow;
use tokio::sync::oneshot;

use pulse_contracts::rpc::Expected;

use super::{Edit, Services, written_revision};
use crate::config::{DefKind, Snapshot, bare_name};
use crate::engine::{Control, OsNonces};
use crate::error::{ServiceError, ServiceResult};

fn runtime(rows: &[RuleStateRow], name: &str) -> RuleRuntimeStatus {
    let row = rows.iter().find(|r| r.rule == name);
    RuleRuntimeStatus {
        snoozed_until: row.and_then(|r| r.snoozed_until),
        circuit_open_until: row.and_then(|r| r.circuit_open_until),
        last_fired_at: row.and_then(|r| r.last_fired_at),
    }
}

fn info(snapshot: &Snapshot, rule: &Sourced<RuleConfig>, rows: &[RuleStateRow]) -> RuleInfo {
    let bare = bare_name(&rule.config.name, &rule.origin);
    RuleInfo {
        config: rule.config.clone(),
        origin: rule.origin.clone(),
        revision: written_revision(&snapshot.files, &rule.origin, |f| {
            f.rules.iter().find(|r| r.name == bare)
        }),
        state: runtime(rows, &rule.config.name),
    }
}

impl Services {
    async fn rule_rows(&self) -> ServiceResult<Vec<RuleStateRow>> {
        Ok(self.shared.store.read(|q| q.query().rule_states()).await?)
    }

    pub async fn rules_list(&self, include_archived: bool) -> ServiceResult<RuleList> {
        let snapshot = self.shared.snapshot();
        let rows = self.rule_rows().await?;
        let rules = snapshot
            .config
            .rules
            .iter()
            .filter(|r| include_archived || !r.config.archived)
            .map(|r| info(&snapshot, r, &rows))
            .collect();
        Ok(RuleList { rules })
    }

    pub async fn rules_get(&self, name: &str) -> ServiceResult<RuleInfo> {
        let snapshot = self.shared.snapshot();
        let rule =
            snapshot.config.rule(name).ok_or_else(|| ServiceError::not_found("rule", name))?;
        Ok(info(&snapshot, rule, &self.rule_rows().await?))
    }

    /// Writes the rule into the user file (or a trusted project's file) and reloads.
    pub async fn rules_upsert(&self, params: RuleUpsertParams) -> ServiceResult<RuleInfo> {
        let rule = params.rule;
        validate_name(&rule.name).map_err(ServiceError::Invalid)?;
        let full = match &params.project {
            Some(p) => format!("{p}/{}", rule.name),
            None => rule.name.clone(),
        };
        self.write_definition(
            DefKind::Rule,
            params.project.as_deref(),
            &rule.name,
            &params.expected,
            Edit::Put(&rule),
        )
        .await?;
        self.rules_get(&full).await
    }

    pub async fn rules_delete(&self, name: &str, expected: &Expected) -> ServiceResult<()> {
        let origin = self.origin_of(DefKind::Rule, name, expected)?;
        self.write_definition::<RuleConfig>(
            DefKind::Rule,
            origin.project(),
            bare_name(name, &origin),
            expected,
            Edit::Delete,
        )
        .await?;
        let name = name.to_owned();
        self.shared.store.write(move |s| s.delete_rule_state(&name)).await?;
        Ok(())
    }

    /// Changes one field of a rule as it is in its file now.
    async fn edit_rule(
        &self,
        name: &str,
        change: &(dyn Fn(&mut RuleConfig) + Sync),
    ) -> ServiceResult<RuleInfo> {
        let origin = self.origin_of(DefKind::Rule, name, &Expected::Any)?;
        let bare = bare_name(name, &origin);
        self.write_definition(
            DefKind::Rule,
            origin.project(),
            bare,
            &Expected::Any,
            Edit::Modify(change),
        )
        .await?;
        self.rules_get(name).await
    }

    pub async fn rules_set_enabled(&self, name: &str, enabled: bool) -> ServiceResult<RuleInfo> {
        self.edit_rule(name, &move |r| r.enabled = enabled).await
    }

    pub async fn rules_archive(&self, name: &str) -> ServiceResult<RuleInfo> {
        self.edit_rule(name, &|r| r.archived = true).await
    }

    pub async fn rules_reopen(&self, name: &str) -> ServiceResult<RuleInfo> {
        self.edit_rule(name, &|r| r.archived = false).await
    }

    async fn set_snooze(&self, name: &str, until: Option<Timestamp>) -> ServiceResult<RuleInfo> {
        if self.shared.snapshot().config.rule(name).is_none() {
            return Err(ServiceError::not_found("rule", name));
        }
        let (reply, rx) = oneshot::channel();
        self.shared.engine.control(Control::Snooze { rule: name.to_owned(), until, reply }).await?;
        rx.await.map_err(|_| ServiceError::shutting_down())??;
        self.shared.notify_state(&[pulse_contracts::rpc::StateTopic::Rules]);
        self.rules_get(name).await
    }

    pub async fn rules_snooze(&self, name: &str, until: Timestamp) -> ServiceResult<RuleInfo> {
        if until <= self.shared.clock.now() {
            return Err(ServiceError::Invalid("`until` must be in the future".into()));
        }
        self.set_snooze(name, Some(until)).await
    }

    /// Clears a snooze and a tripped circuit breaker.
    pub async fn rules_unsnooze(&self, name: &str) -> ServiceResult<RuleInfo> {
        self.set_snooze(name, None).await
    }

    /// Routes a past event through one rule on a copy of the routing state: what would happen,
    /// and the prompt that would be sent. No side effects.
    pub async fn rules_test(&self, params: RuleTestParams) -> ServiceResult<RuleTestResult> {
        let event_id = params.event;
        let event = self
            .shared
            .store
            .read(move |q| q.query().event(event_id))
            .await?
            .ok_or_else(|| ServiceError::not_found("event", event_id))?;
        let snapshot = self.shared.snapshot();
        let (routing, name) = match &params.rule {
            RuleRef::Saved { name } => {
                if snapshot.config.rule(name).is_none() {
                    return Err(ServiceError::not_found("rule", name));
                }
                (None, name.clone())
            }
            RuleRef::Draft { rule, project } => {
                validate_name(&rule.name).map_err(ServiceError::Invalid)?;
                let (_, routing, full) =
                    snapshot.with_draft(rule, project.as_deref(), &self.shared.user_home)?;
                (Some(routing), full)
            }
        };
        let routing = routing.as_ref().unwrap_or(&snapshot.routing);
        let (reply, rx) = oneshot::channel();
        self.shared.engine.control(Control::CopyState(reply)).await?;
        let mut state = rx.await.map_err(|_| ServiceError::shutting_down())?;
        if params.ignore_runtime_state {
            state.restore_rule(&name, RuleRuntime::default());
        }
        let cause_rule = match event.causation_run {
            Some(run) => {
                self.shared.store.read(move |q| q.query().run(run)).await?.and_then(|r| r.rule)
            }
            None => None,
        };
        let cause = event.causation_run.map(|run| Cause { run, rule: cause_rule.as_deref() });
        let mut nonces = OsNonces;
        let mut ctx = RouteContext {
            now: self.shared.clock.now(),
            paused: self.shared.paused(),
            cause,
            nonces: &mut nonces,
        };
        let only = [name.clone()];
        let decision =
            route(&event, RouteMode::Only(&only), routing, &mut state, &mut ctx).into_iter().next();
        let after_debounce = match &decision {
            Some(d) if matches!(d.outcome, Outcome::Debounced { .. }) => {
                route(&event, RouteMode::DebounceElapsed(&name), routing, &mut state, &mut ctx)
                    .into_iter()
                    .next()
            }
            _ => None,
        };
        Ok(RuleTestResult {
            matched: routing.rule_matches(&name, &event),
            decision,
            after_debounce,
        })
    }

    /// The rule's deliveries, and for each loop-protection block how its event's cause was
    /// known (looked up on the event: at most one page of point reads).
    pub async fn rules_history(&self, params: RuleHistoryParams) -> ServiceResult<DeliveryPage> {
        Ok(self
            .shared
            .store
            .read(move |q| {
                let q = q.query();
                let mut page =
                    q.rule_history(&params.name, &params.outcomes, params.cursor, params.limit)?;
                for d in &page.deliveries {
                    let loop_block = matches!(
                        &d.outcome,
                        DeliveryOutcome::Blocked {
                            reason: BlockReason::LoopSelfTrigger { .. }
                                | BlockReason::LoopChainDepth { .. }
                        }
                    );
                    if loop_block && !page.causes.contains_key(&d.event) {
                        if let Some(kind) = q.event(d.event)?.and_then(|e| e.causation) {
                            page.causes.insert(d.event, kind);
                        }
                    }
                }
                Ok(page)
            })
            .await?)
    }
}
