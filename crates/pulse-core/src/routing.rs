//! `route()`: which rules an event fires, and why the others did not.
//!
//! ## Order of checks
//!
//! For each rule in config order (archived rules are skipped entirely):
//!
//! 1. **Match**: one of `on`'s type globs, and the `where` clause. No match, no decision.
//!    Every later step produces a decision, which the daemon records as a delivery.
//! 2. **Rule state**: disabled → `rule_disabled`; global pause → `global_pause`; snoozed →
//!    `rule_snoozed`; circuit breaker open → `circuit_open`.
//! 3. **Permission**: the agent's scopes must cover the event → `permission_denied`.
//! 4. **Loop guard**: caused by a run of this same rule → `loop_self_trigger`; the run would
//!    exceed `max_chain_depth` → `loop_chain_depth`.
//! 5. **Debounce** (rules with `debounce`): the event is held → `debounced`, and fires when the
//!    timer elapses (the daemon calls `route` again with [`RouteMode::DebounceElapsed`], which
//!    re-runs steps 1–4 and skips this one). If the debouncer is full, the event skips debouncing.
//! 6. **Cooldown**, then **rate limit** → `cooldown` / `rate_limited`. For an elapsed debounce,
//!    these re-arm the debounce until the limit lifts instead of dropping the event.
//! 7. **Circuit breaker**: one more fire would exceed `max_fires` within the window → the
//!    breaker opens for `open_for` → `circuit_open`. Only fires since the breaker last closed
//!    (its hold ended, or `unsnooze`) count, so closing it is a fresh start, not a re-trip at
//!    the next match.
//! 8. **Agent backlog** (unsupervised rules): queued + running runs of the agent at
//!    `max_concurrent + max_queued` → `agent_busy`. Concurrency itself is enforced by the
//!    effect worker; queued runs wait for a slot.
//! 9. **Render** the prompt → `render_failed` on error.
//! 10. **Supervised** rules → `await_approval`; others → `fire`. Both count as fires for
//!     cooldown, rate limit and breaker.
//!
//! Cheap, state-only checks come first so that a paused or disabled rule never touches the
//! debouncer or limits; permission and loop checks precede the debounce so a blocked event never
//! occupies a debounce slot.
//!
//! ## Purity
//!
//! `route` does no I/O. Time, pause, causation and randomness arrive in [`RouteContext`]; all
//! mutable state is the [`RouterState`] the caller passes. A dry run ("test this rule against a
//! past event") calls `route` on a clone of the state.

use std::collections::BTreeMap;
use std::path::Path;

use pulse_contracts::config::{
    AgentKind, Config, ConfigError, ConfigErrors, DebounceBy, Limits, Rate,
};
use pulse_contracts::pattern::{Glob, parse_type_glob};
use pulse_contracts::template::Template;
use pulse_contracts::{BlockReason, Decision, Duration, Event, EventId, Outcome, RunId, Timestamp};

use crate::debounce::Debouncer;
use crate::glob;
use crate::limits::{
    FireHistory, breaker_trips, cooldown_until, history_capacity, rate_limited_until,
};
use crate::matcher::CompiledWhere;
use crate::prompt::{NonceSource, RenderInput, render_prompt};
use crate::scope::ScopeSet;

#[derive(Clone, Debug)]
struct CompiledRule {
    name: String,
    agent: String,
    types: Vec<Glob>,
    where_: CompiledWhere,
    debounce: Option<Duration>,
    debounce_by: DebounceBy,
    cooldown: Option<Duration>,
    rate_limit: Option<Rate>,
    supervised: bool,
    enabled: bool,
    archived: bool,
    prompt: Option<Template>,
}

#[derive(Clone, Debug)]
struct CompiledAgent {
    kind: AgentKind,
    scopes: ScopeSet,
    max_concurrent: u32,
    max_queued: u32,
}

/// The routing view of a [`Config`]: rules and agents with their globs, matchers, scopes and
/// templates compiled. Rebuild it on every config change.
#[derive(Clone, Debug)]
pub struct Routing {
    rules: Vec<CompiledRule>,
    agents: BTreeMap<String, CompiledAgent>,
    limits: Limits,
}

impl Routing {
    /// Compiles a resolved config. `home` expands `~` in path globs and scopes.
    pub fn compile(config: &Config, home: &Path) -> Result<Routing, ConfigErrors> {
        let mut errors = Vec::new();
        let mut err = |item: String,
                       field: &str,
                       message: String,
                       origin: &pulse_contracts::config::ConfigOrigin| {
            errors.push(ConfigError {
                file: Some(origin.path().display().to_string()),
                line: None,
                column: None,
                item: Some(item),
                field: Some(field.to_owned()),
                message,
            });
        };
        let mut agents = BTreeMap::new();
        for a in &config.agents {
            match ScopeSet::compile(&a.config.allow, home) {
                Ok(scopes) => {
                    agents.insert(
                        a.config.name.clone(),
                        CompiledAgent {
                            kind: a.config.kind(),
                            scopes,
                            max_concurrent: a.config.max_concurrent,
                            max_queued: a.config.max_queued,
                        },
                    );
                }
                Err(e) => {
                    err(format!("agent {:?}", a.config.name), "allow", e.to_string(), &a.origin)
                }
            }
        }
        let mut rules = Vec::new();
        for r in &config.rules {
            let c = &r.config;
            let item = format!("rule {:?}", c.name);
            let types: Result<Vec<Glob>, _> =
                c.on.0.iter().map(|t| parse_type_glob(t.as_str())).collect();
            let where_ = CompiledWhere::compile(&c.where_, home);
            let prompt = c.run.prompt.as_deref().map(Template::parse).transpose();
            match (types, where_, prompt) {
                (Ok(types), Ok(where_), Ok(prompt)) => rules.push(CompiledRule {
                    name: c.name.clone(),
                    agent: c.run.agent.clone(),
                    types,
                    where_,
                    debounce: c.debounce,
                    debounce_by: c.debounce_by,
                    cooldown: c.cooldown,
                    rate_limit: c.rate_limit,
                    supervised: c.supervised,
                    enabled: c.enabled,
                    archived: c.archived,
                    prompt,
                }),
                (Err(e), _, _) => err(item, "on", e.to_string(), &r.origin),
                (_, Err(e), _) => err(item, "where", e.to_string(), &r.origin),
                (_, _, Err(e)) => err(item, "run.prompt", e.to_string(), &r.origin),
            }
        }
        if errors.is_empty() {
            Ok(Routing { rules, agents, limits: config.limits.clone() })
        } else {
            Err(ConfigErrors(errors))
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub fn rule_names(&self) -> impl Iterator<Item = &str> {
        self.rules.iter().map(|r| r.name.as_str())
    }

    /// Whether `rule` (not archived) matches the event's type and `where` clause.
    pub fn rule_matches(&self, rule: &str, event: &Event) -> bool {
        self.rules.iter().any(|r| r.name == rule && !r.archived && rule_matches(r, event))
    }

    /// Checks an event against an agent's scopes (for MCP queries and subscriptions). Unknown
    /// agents are denied.
    pub fn agent_permits(&self, agent: &str, event: &Event) -> bool {
        self.agents.get(agent).is_some_and(|a| a.scopes.permits(event).is_ok())
    }

    /// Whether `agent` could ever receive events of a catalog type (`ScopeSet::may_permit`).
    /// Unknown agents receive nothing.
    pub fn agent_may_receive(&self, agent: &str, catalog_type: &str, carries_paths: bool) -> bool {
        self.agents.get(agent).is_some_and(|a| a.scopes.may_permit(catalog_type, carries_paths))
    }

    pub fn agent_kind(&self, agent: &str) -> Option<AgentKind> {
        self.agents.get(agent).map(|a| a.kind)
    }
}

fn rule_matches(rule: &CompiledRule, event: &Event) -> bool {
    rule.types.iter().any(|g| glob::matches(g, event.type_.as_str())) && rule.where_.matches(event)
}

/// Per-rule runtime state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleRuntime {
    pub snoozed_until: Option<Timestamp>,
    /// When the circuit breaker's latest hold ends (or ended): open while it is in the future.
    /// Once past, it is when the breaker closed, and the breaker counts only later fires; an
    /// unsnooze sets it to the moment of the unsnooze. Kept after it passes for that reason.
    pub circuit_open_until: Option<Timestamp>,
    pub fires: FireHistory,
}

/// What a debounce timer is keyed by.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DebounceKey {
    pub rule: String,
    /// Set for `debounce_by = "subject"`.
    pub subject: Option<String>,
}

/// An elapsed debounce: route `event` for `rule` with [`RouteMode::DebounceElapsed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueDebounce {
    pub rule: String,
    pub event: EventId,
}

/// Everything `route` remembers between calls. Bounded: one entry per configured rule and agent,
/// and a capacity-limited debouncer.
#[derive(Clone, Debug)]
pub struct RouterState {
    rules: BTreeMap<String, RuleRuntime>,
    debouncer: Debouncer<DebounceKey, EventId>,
    agent_backlog: BTreeMap<String, u32>,
}

impl RouterState {
    pub fn new(limits: &Limits) -> Self {
        RouterState {
            rules: BTreeMap::new(),
            debouncer: Debouncer::new(limits.debounce_capacity as usize),
            agent_backlog: BTreeMap::new(),
        }
    }

    pub fn rule(&self, name: &str) -> Option<&RuleRuntime> {
        self.rules.get(name)
    }

    /// Restores persisted state at startup (snooze and breaker from `rule_state`; recent fire
    /// times from the delivery log).
    pub fn restore_rule(&mut self, name: &str, runtime: RuleRuntime) {
        self.rules.insert(name.to_owned(), runtime);
    }

    pub fn snooze(&mut self, rule: &str, until: Timestamp) {
        self.rules.entry(rule.to_owned()).or_default().snoozed_until = Some(until);
    }

    /// Clears a snooze and closes an open circuit breaker. A breaker closed by hand counts
    /// fires afresh from `now`, as it does when its hold ends: the fires that tripped it are
    /// still in its window and would trip it again at the next match. Cooldown and rate limit
    /// still count them.
    pub fn unsnooze(&mut self, rule: &str, now: Timestamp) {
        if let Some(r) = self.rules.get_mut(rule) {
            r.snoozed_until = None;
            if r.circuit_open_until.is_some_and(|u| u > now) {
                r.circuit_open_until = Some(now);
            }
        }
    }

    /// The number of queued plus running runs of `agent`. The daemon keeps this current as runs
    /// start and finish; `route` increments it for each unsupervised fire.
    pub fn set_agent_backlog(&mut self, agent: &str, backlog: u32) {
        self.agent_backlog.insert(agent.to_owned(), backlog);
    }

    pub fn agent_backlog(&self, agent: &str) -> u32 {
        self.agent_backlog.get(agent).copied().unwrap_or(0)
    }

    /// When the caller should next call [`RouterState::take_due`]. Arm exactly one timer.
    pub fn next_deadline(&self) -> Option<Timestamp> {
        self.debouncer.next_deadline()
    }

    pub fn take_due(&mut self, now: Timestamp) -> Vec<DueDebounce> {
        self.debouncer
            .pop_due(now)
            .into_iter()
            .map(|(key, event)| DueDebounce { rule: key.rule, event })
            .collect()
    }

    /// Drops state for rules and agents no longer configured. Call after a config reload.
    pub fn retain_configured(&mut self, routing: &Routing) {
        let names: std::collections::BTreeSet<&str> = routing.rule_names().collect();
        self.rules.retain(|k, _| names.contains(k.as_str()));
        self.debouncer.retain(|k| names.contains(k.rule.as_str()));
        self.agent_backlog.retain(|k, _| routing.agents.contains_key(k));
    }
}

/// The run an event is attributed to, as the loop guard needs it.
#[derive(Clone, Copy, Debug)]
pub struct Cause<'a> {
    pub run: RunId,
    /// The causing run's rule, if it had one.
    pub rule: Option<&'a str>,
}

/// Inputs to one `route` call that are not state.
pub struct RouteContext<'a> {
    pub now: Timestamp,
    /// Global pause.
    pub paused: bool,
    /// The run the event is attributed to (`pulsecausationrun`), resolved by the caller.
    pub cause: Option<Cause<'a>>,
    pub nonces: &'a mut dyn NonceSource,
}

impl std::fmt::Debug for RouteContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteContext")
            .field("now", &self.now)
            .field("paused", &self.paused)
            .field("cause", &self.cause)
            .finish()
    }
}

/// Which rules to evaluate.
#[derive(Clone, Copy, Debug)]
pub enum RouteMode<'a> {
    /// Every rule: a new event.
    AllRules,
    /// Only these rules: replays restricted to some rules, and dry runs.
    Only(&'a [String]),
    /// A debounce for this rule elapsed; the event is its latest.
    DebounceElapsed(&'a str),
}

/// Routes one event. Returns one decision per matching rule, in config order. See the module
/// docs for the order of checks.
pub fn route(
    event: &Event,
    mode: RouteMode<'_>,
    routing: &Routing,
    state: &mut RouterState,
    ctx: &mut RouteContext<'_>,
) -> Vec<Decision> {
    let mut decisions = Vec::new();
    for rule in &routing.rules {
        let elapsed = match mode {
            RouteMode::AllRules => false,
            RouteMode::Only(names) => {
                if !names.contains(&rule.name) {
                    continue;
                }
                false
            }
            RouteMode::DebounceElapsed(name) => {
                if rule.name != name {
                    continue;
                }
                true
            }
        };
        if rule.archived || !rule_matches(rule, event) {
            continue;
        }
        let outcome = decide(rule, event, elapsed, routing, state, ctx);
        decisions.push(Decision {
            rule: rule.name.clone(),
            agent: rule.agent.clone(),
            outcome,
            chain_depth: event.chain_depth.saturating_add(1),
        });
    }
    decisions
}

fn blocked(reason: BlockReason) -> Outcome {
    Outcome::Blocked { reason }
}

fn decide(
    rule: &CompiledRule,
    event: &Event,
    debounce_elapsed: bool,
    routing: &Routing,
    state: &mut RouterState,
    ctx: &mut RouteContext<'_>,
) -> Outcome {
    let now = ctx.now;
    let limits = &routing.limits;

    // 2. Rule state.
    if !rule.enabled {
        return blocked(BlockReason::RuleDisabled);
    }
    if ctx.paused {
        return blocked(BlockReason::GlobalPause);
    }
    let runtime = state.rules.entry(rule.name.clone()).or_default();
    if let Some(until) = runtime.snoozed_until.filter(|u| *u > now) {
        return blocked(BlockReason::RuleSnoozed { until });
    }
    if let Some(until) = runtime.circuit_open_until.filter(|u| *u > now) {
        return blocked(BlockReason::CircuitOpen { until });
    }

    // 3. Permission. Config resolution guarantees the agent exists; a missing one is denied.
    let Some(agent) = routing.agents.get(&rule.agent) else {
        return blocked(BlockReason::PermissionDenied { needed: event.type_.to_string() });
    };
    if let Err(denied) = agent.scopes.permits(event) {
        return blocked(BlockReason::PermissionDenied { needed: denied.needed });
    }

    // 4. Loop guard.
    if let Some(cause) = ctx.cause.filter(|c| c.rule == Some(rule.name.as_str())) {
        return blocked(BlockReason::LoopSelfTrigger { run: cause.run });
    }
    let depth = event.chain_depth.saturating_add(1);
    if depth > limits.max_chain_depth {
        return blocked(BlockReason::LoopChainDepth { depth, max: limits.max_chain_depth });
    }

    // 5. Debounce.
    let key = || DebounceKey {
        rule: rule.name.clone(),
        subject: match rule.debounce_by {
            DebounceBy::Rule => None,
            DebounceBy::Subject => event.subject.clone(),
        },
    };
    if let (Some(d), false) = (rule.debounce, debounce_elapsed) {
        let until = now.saturating_add(d);
        if state.debouncer.push(key(), event.id, until).is_ok() {
            return Outcome::Debounced { until };
        }
        // Full: fall through without debouncing.
    }

    // 6. Cooldown and rate limit.
    let runtime = state.rules.entry(rule.name.clone()).or_default();
    let limited = rule
        .cooldown
        .and_then(|cd| {
            cooldown_until(cd, &runtime.fires, now).map(|until| BlockReason::Cooldown { until })
        })
        .or_else(|| {
            rule.rate_limit.and_then(|r| {
                rate_limited_until(r, &runtime.fires, now)
                    .map(|until| BlockReason::RateLimited { until })
            })
        });
    if let Some(reason) = limited {
        if debounce_elapsed {
            let until = match reason {
                BlockReason::Cooldown { until } | BlockReason::RateLimited { until } => until,
                _ => now,
            };
            // A newer event may already be pending for this key; it wins.
            let key = key();
            if state.debouncer.contains(&key) {
                return Outcome::Debounced {
                    until: state.debouncer.deadline(&key).unwrap_or(until),
                };
            }
            if state.debouncer.push(key, event.id, until).is_ok() {
                return Outcome::Debounced { until };
            }
        }
        return blocked(reason);
    }

    // 7. Circuit breaker.
    let breaker = &limits.circuit_breaker;
    if breaker_trips(breaker, &runtime.fires, runtime.circuit_open_until, now) {
        let until = now.saturating_add(breaker.open_for);
        runtime.circuit_open_until = Some(until);
        return blocked(BlockReason::CircuitOpen { until });
    }

    // 8. Agent backlog.
    let max = agent.max_concurrent.saturating_add(agent.max_queued);
    let backlog = state.agent_backlog(&rule.agent);
    if !rule.supervised && backlog >= max {
        return blocked(BlockReason::AgentBusy { backlog, max });
    }

    // 9. Render.
    let prompt = match &rule.prompt {
        None => None,
        Some(t) => {
            let input = RenderInput { event, rule: &rule.name, agent: &rule.agent };
            match render_prompt(t, &input, &limits.prompt, ctx.nonces) {
                Ok(p) => Some(p),
                Err(e) => return blocked(BlockReason::RenderFailed { message: e.to_string() }),
            }
        }
    };

    // 10. Fire.
    let capacity = history_capacity(rule.rate_limit, breaker);
    if let Some(r) = state.rules.get_mut(&rule.name) {
        r.fires.record(now, capacity);
    }
    if rule.supervised {
        Outcome::AwaitApproval { prompt }
    } else {
        *state.agent_backlog.entry(rule.agent.clone()).or_default() += 1;
        Outcome::Fire { prompt }
    }
}

#[cfg(test)]
mod tests;
