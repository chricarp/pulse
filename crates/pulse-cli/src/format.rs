//! Text output.

use pulse_contracts::run::RunOutputChunk;
use pulse_contracts::{
    BlockReason, Causation, Decision, Delivery, DeliveryOutcome, Event, Outcome, OutputKind,
    RunStatus, RunSummary,
};

/// Event content shown on a terminal: escape sequences could recolour or retitle the terminal,
/// write the clipboard (OSC 52) or hide text, bidi overrides make `txt.exe` read as `exe.txt`,
/// and tag characters carry text nobody sees. Control and invisible formatting characters are
/// shown escaped (`\u{1b}`); newlines and tabs are kept.
pub fn terminal_safe(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(unsafe_char) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if unsafe_char(c) {
            out.push_str(&c.escape_unicode().to_string());
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Controls, and the invisible formatting characters prompt rendering removes (one list, in
/// `pulse_core::prompt::is_invisible`; the web UI and the desktop shell keep their own copies).
fn unsafe_char(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || pulse_core::prompt::is_invisible(c)
}

/// One line of event content: also keeps a newline in a file name from starting a new line.
fn one_line(s: &str) -> String {
    terminal_safe(s).replace('\n', "\\n").replace('\t', "\\t")
}

pub fn event_line(e: &Event) -> String {
    let subject = e.subject.as_deref().map_or_else(|| "-".to_owned(), one_line);
    let mut line = format!("{}  {:<24} {subject}", e.time, e.type_);
    if let Some(run) = e.causation_run {
        // Exact: the run said so itself. Reported: its agent's tool call named the path.
        // Inferred: it happened in the run's folder while the run's writes were not visible.
        let how = e.causation.map_or("", |c| match c {
            pulse_contracts::Causation::Exact => "",
            pulse_contracts::Causation::Reported => ", reported",
            pulse_contracts::Causation::Inferred => ", inferred",
        });
        line.push_str(&format!("  (from run {run}{how})"));
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
        Outcome::Waiting { reason: r } => format!("waiting: {}; fires when a run ends", reason(r)),
        Outcome::Blocked { reason: r } => format!("blocked: {}", reason(r)),
    }
}

pub fn delivery_outcome(o: &DeliveryOutcome) -> String {
    match o {
        DeliveryOutcome::Fired => "fired".into(),
        DeliveryOutcome::AwaitingApproval => "awaiting approval".into(),
        DeliveryOutcome::Debounced { until } => format!("debounced until {until}"),
        DeliveryOutcome::Waiting { reason: r } => {
            format!("waiting: {}; fires when a run ends", reason(r))
        }
        DeliveryOutcome::Blocked { reason: r } => format!("blocked: {}", reason(r)),
    }
}

/// How an event's causing run is known, for people.
pub fn causation(c: Causation) -> &'static str {
    match c {
        Causation::Exact => "exact: the run sent it itself",
        Causation::Reported => "reported: its agent's tool call named the path",
        Causation::Inferred => {
            "inferred: it appeared in the run's working directory while the run's writes were not visible"
        }
    }
}

/// One routing decision. `cause` is how its event's causing run is known (it is recorded on
/// the event), shown on a loop-protection block. `run_status` is what became of the run the
/// decision created, when the caller knows: a supervised delivery is recorded as "awaiting
/// approval" for good, and its run says whether it was approved.
pub fn delivery_line(
    d: &Delivery,
    cause: Option<Causation>,
    run_status: Option<RunStatus>,
) -> String {
    let run = d
        .run
        .map(|r| match run_status {
            Some(s) => format!("  run {r}: now {}", s.as_str()),
            None => format!("  run {r}"),
        })
        .unwrap_or_default();
    let loop_block = matches!(
        &d.outcome,
        DeliveryOutcome::Blocked {
            reason: BlockReason::LoopSelfTrigger { .. } | BlockReason::LoopChainDepth { .. }
        }
    );
    let how =
        cause.filter(|_| loop_block).map(|c| format!(" ({})", causation(c))).unwrap_or_default();
    let mut line = format!(
        "{}  {:<20} {}{how}{run}  [event {}]",
        d.at,
        d.rule,
        delivery_outcome(&d.outcome),
        d.event
    );
    // Attribution can be inferred (a file appeared in the working directory of a run whose
    // writes Pulse could not see), and then a change the user made is taken for the agent's:
    // say how to undo that.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_content_cannot_drive_the_terminal() {
        let cases = [
            ("plain.pdf", "plain.pdf"),
            ("esc\x1b[31mred", "esc\\u{1b}[31mred"),
            ("osc\x1b]52;c;aGk=\x07", "osc\\u{1b}]52;c;aGk=\\u{7}"),
            ("name\u{202e}txt.exe", "name\\u{202e}txt.exe"),
            ("zero\u{200b}width", "zero\\u{200b}width"),
            ("c1\u{9b}31m", "c1\\u{9b}31m"),
            ("line\nbreak\ttab", "line\nbreak\ttab"),
            ("è unicode ✓", "è unicode ✓"),
        ];
        for (raw, shown) in cases {
            assert_eq!(terminal_safe(raw), shown, "{raw:?}");
        }
        assert_eq!(one_line("new\nline.txt"), "new\\nline.txt");
    }

    #[test]
    fn a_delivery_says_what_became_of_its_run() {
        use pulse_contracts::{DeliveryId, EventId, RunId, Timestamp, Ulid};
        let d = Delivery {
            id: DeliveryId::from_ulid(Ulid::from_parts(1, 0)),
            event: EventId::from_ulid(Ulid::from_parts(1, 1)),
            rule: "careful".into(),
            agent: "a".into(),
            at: Timestamp::from_millis(0),
            outcome: DeliveryOutcome::AwaitingApproval,
            run: Some(RunId::from_ulid(Ulid::from_parts(1, 2))),
        };
        let approved = delivery_line(&d, None, Some(RunStatus::Succeeded));
        assert!(approved.contains("awaiting approval"), "the decision stays: {approved}");
        assert!(approved.contains(": now succeeded"), "{approved}");
        assert!(!delivery_line(&d, None, None).contains("now"));
    }

    /// The cases of `pulse-core/src/prompt/tests.rs`, `apps/web/src/lib/format.test.ts` and
    /// `apps/desktop/src/policy.test.ts`; the lists must agree.
    #[test]
    fn hidden_characters_are_shown_escaped() {
        const HIDDEN: &[u32] = &[
            0x00AD, 0x034F, 0x061C, 0x115F, 0x1160, 0x17B4, 0x17B5, 0x180E, 0x200B, 0x200C, 0x200D,
            0x200E, 0x200F, 0x2028, 0x2029, 0x202A, 0x202E, 0x2060, 0x2064, 0x2065, 0x2066, 0x2069,
            0x206A, 0x206F, 0x3164, 0xFEFF, 0xFFA0, 0xFFF0, 0xFFF9, 0xFFFB, 0x1BCA0, 0x1BCA3,
            0x1D173, 0x1D17A, 0xE0000, 0xE0001, 0xE0041, 0xE007F, 0xE0100, 0xE01EF, 0xE0FFF,
        ];
        const KEPT: &[u32] = &[0x0041, 0x00E9, 0x0301, 0x2764, 0xFE0F, 0x1F600, 0x4E2D];
        for &cp in HIDDEN {
            let c = char::from_u32(cp).unwrap();
            assert_eq!(terminal_safe(&format!("a{c}b")), format!("a\\u{{{cp:x}}}b"), "U+{cp:04X}");
        }
        for &cp in KEPT {
            let s = format!("a{}b", char::from_u32(cp).unwrap());
            assert_eq!(terminal_safe(&s), s, "U+{cp:04X}");
        }
    }
}
