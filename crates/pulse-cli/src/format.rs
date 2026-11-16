//! Text output.

use pulse_contracts::run::RunOutputChunk;
use pulse_contracts::{
    BlockReason, Decision, Delivery, DeliveryOutcome, Event, Outcome, OutputKind, RunSummary,
};

pub fn event_line(e: &Event) -> String {
    let mut line = format!("{}  {:<24} {}", e.time, e.type_, e.subject.as_deref().unwrap_or("-"));
    if let Some(run) = e.causation_run {
        let how = e.causation.map_or("", |c| match c {
            pulse_contracts::Causation::Exact => "",
            pulse_contracts::Causation::Inferred => "~",
        });
        line.push_str(&format!("  (from run {how}{run})"));
    }
    if let Some(orig) = e.replay_of {
        line.push_str(&format!("  (replay of {orig})"));
    }
    line.push_str(&format!("  [{}]", e.id));
    line
}

pub fn run_line(r: &RunSummary) -> String {
    let mut line = format!(
        "{}  {:<16} rule={} agent={}",
        r.id,
        r.status.as_str(),
        r.rule.as_deref().unwrap_or("-"),
        r.agent
    );
    if let Some(code) = r.exit_code {
        line.push_str(&format!(" exit={code}"));
    }
    if r.chain_depth > 1 {
        line.push_str(&format!(" depth={}", r.chain_depth));
    }
    line
}

pub fn reason(r: &BlockReason) -> String {
    match r {
        BlockReason::RuleDisabled => "the rule is disabled".into(),
        BlockReason::RuleSnoozed { until } => format!("the rule is snoozed until {until}"),
        BlockReason::GlobalPause => "Pulse is paused".into(),
        BlockReason::PermissionDenied { needed } => {
            format!("the agent's scopes do not cover it (it would need \"{needed}\")")
        }
        BlockReason::LoopSelfTrigger { run } => {
            format!("loop protection: the event was attributed to this rule's own run {run}")
        }
        BlockReason::LoopChainDepth { depth, max } => {
            format!("loop protection: a chain of {depth} runs exceeds max_chain_depth {max}")
        }
        BlockReason::CircuitOpen { until } => {
            format!("the circuit breaker holds the rule until {until} (it fired too often)")
        }
        BlockReason::Cooldown { until } => format!("cooling down until {until}"),
        BlockReason::RateLimited { until } => format!("rate limited until {until}"),
        BlockReason::AgentBusy { backlog, max } => {
            format!("the agent's queue is full ({backlog} of {max})")
        }
        BlockReason::RenderFailed { message } => {
            format!("the prompt could not be rendered: {message}")
        }
    }
}

pub fn outcome(o: &Outcome) -> String {
    match o {
        Outcome::Fire { .. } => "fire: a run would start".into(),
        Outcome::AwaitApproval { .. } => "await approval: a run would wait for you".into(),
        Outcome::Debounced { until } => format!("debounced until {until}"),
        Outcome::Blocked { reason: r } => format!("blocked: {}", reason(r)),
    }
}

pub fn delivery_outcome(o: &DeliveryOutcome) -> String {
    match o {
        DeliveryOutcome::Fired => "fired".into(),
        DeliveryOutcome::AwaitingApproval => "awaiting approval".into(),
        DeliveryOutcome::Debounced { until } => format!("debounced until {until}"),
        DeliveryOutcome::Blocked { reason: r } => format!("blocked: {}", reason(r)),
    }
}

pub fn delivery_line(d: &Delivery) -> String {
    let run = d.run.map(|r| format!("  run {r}")).unwrap_or_default();
    let mut line = format!(
        "{}  {:<20} {}{run}  [event {}]",
        d.at,
        d.rule,
        delivery_outcome(&d.outcome),
        d.event
    );
    // Attribution can be inferred (a file appeared in the run's working directory while it
    // ran), and then a change the user made is taken for the agent's: say how to undo that.
    if matches!(
        &d.outcome,
        DeliveryOutcome::Blocked { reason: BlockReason::LoopSelfTrigger { .. } }
    ) {
        line.push_str(&format!("\n    if you made this change yourself: pulse replay {}", d.event));
    }
    line
}

pub fn decision(d: &Decision) -> String {
    format!("{} -> {}: {}", d.rule, d.agent, outcome(&d.outcome))
}

pub fn prompt_of(o: &Outcome) -> Option<&str> {
    match o {
        Outcome::Fire { prompt } | Outcome::AwaitApproval { prompt } => {
            prompt.as_ref().map(|p| p.text.as_str())
        }
        _ => None,
    }
}

/// A chunk of run output as a terminal line. Returns whether it belongs on stderr.
pub fn chunk(c: &RunOutputChunk) -> (String, bool) {
    let o = &c.output;
    match o.kind {
        OutputKind::Stdout => (o.text.clone(), false),
        OutputKind::Stderr => (o.text.clone(), true),
        OutputKind::Marker => (o.text.clone(), true),
        other => (format!("[{}] {}", other.as_str(), o.text), false),
    }
}
