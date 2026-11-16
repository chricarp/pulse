use std::path::{Path, PathBuf};

use pulse_contracts::config::{ConfigOrigin, Sourced, parse_config_file, resolve};
use pulse_contracts::{EventType, RunId, Ulid};
use serde_json::{Value, json};

use super::*;

const HOME: &str = "/home/me";

struct Counter(u8);
impl NonceSource for Counter {
    fn nonce(&mut self) -> [u8; 16] {
        self.0 = self.0.wrapping_add(1);
        [self.0; 16]
    }
}

const AGENTS: &str = r#"
[[agent]]
name = "study"
adapter = "claude-code"
allow = ["fs.*:~/University/**", "calendar.*"]

[[agent]]
name = "solo"
adapter = "claude-code"
allow = ["fs.*"]
max_concurrent = 1
max_queued = 0

[[agent]]
name = "notify"
adapter = "command"
command = ["notify-send", "{{subject}}"]
allow = ["*"]
"#;

fn routing(rules: &str, limits: &str) -> Routing {
    let origin = ConfigOrigin::User { path: PathBuf::from("/home/me/.pulse/pulse.toml") };
    let text = format!("{limits}\n{AGENTS}\n{rules}");
    let file = parse_config_file(&text, &origin, Path::new(HOME)).unwrap_or_else(|e| panic!("{e}"));
    let config =
        resolve(Sourced { origin, config: file }, vec![]).unwrap_or_else(|e| panic!("{e}"));
    Routing::compile(&config, Path::new(HOME)).unwrap()
}

fn ev(n: u64, type_: &str, subject: &str, data: Value) -> Event {
    Event {
        specversion: Default::default(),
        id: EventId::from_ulid(Ulid::from_parts(n, 0)),
        type_: EventType::new(type_).unwrap(),
        source: "pulse://fs/university".into(),
        subject: Some(subject.into()),
        time: Timestamp::from_millis(0),
        datacontenttype: None,
        data,
        dedupe_key: None,
        coalesced: None,
        causation_run: None,
        causation: None,
        chain_depth: 0,
        replay_of: None,
    }
}

fn pdf(n: u64, name: &str) -> Event {
    let path = format!("/home/me/University/{name}");
    ev(n, "fs.file.created", &path, json!({ "path": path }))
}

fn t(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

struct Ctx {
    now: i64,
    paused: bool,
    cause_rule: Option<&'static str>,
}

fn run_route(
    event: &Event,
    mode: RouteMode<'_>,
    r: &Routing,
    state: &mut RouterState,
    c: Ctx,
) -> Vec<Decision> {
    let mut nonces = Counter(0);
    let run = RunId::from_ulid(Ulid::from_parts(99, 0));
    let mut ctx = RouteContext {
        now: t(c.now),
        paused: c.paused,
        cause: c.cause_rule.map(|rule| Cause { run, rule: Some(rule) }),
        nonces: &mut nonces,
    };
    route(event, mode, r, state, &mut ctx)
}

fn at(now: i64) -> Ctx {
    Ctx { now, paused: false, cause_rule: None }
}

/// A short label for an outcome, for readable tables.
fn label(o: &Outcome) -> String {
    match o {
        Outcome::Fire { prompt } => {
            format!("fire{}", if prompt.is_some() { "" } else { "(no prompt)" })
        }
        Outcome::AwaitApproval { .. } => "await_approval".into(),
        Outcome::Debounced { until } => format!("debounced until {}", until.as_millis()),
        Outcome::Blocked { reason } => format!("{reason:?}"),
    }
}

const RULE: &str = r#"
[[rule]]
name = "lecture"
on = "fs.file.*"
where = { path = "~/University/**/*.pdf" }
run = { agent = "study", prompt = "Summarize {{subject}}" }
"#;

#[test]
#[allow(clippy::type_complexity)]
fn order_of_checks() {
    let rule = |extra: &str| format!("{RULE}{extra}");
    let run_id = RunId::from_ulid(Ulid::from_parts(99, 0));
    let a = pdf(1, "a.pdf");
    let outside =
        ev(2, "fs.file.created", "/home/me/Secret/a.pdf", json!({"path": "/home/me/Secret/a.pdf"}));
    // (rule extras, limits, setup on state, event, ctx, expected labels)
    type Setup = fn(&mut RouterState);
    let none: Setup = |_| {};
    let cases: Vec<(String, &str, Setup, Event, Ctx, Vec<String>)> = vec![
        (rule(""), "", none, a.clone(), at(0), vec!["fire".into()]),
        // 1. No match: no decision at all.
        (rule(""), "", none, ev(3, "git.commit.created", "/r", json!({})), at(0), vec![]),
        (rule(""), "", none, pdf(4, "a.txt"), at(0), vec![]),
        (rule("archived = true"), "", none, a.clone(), at(0), vec![]),
        // 2. Rule state, disabled first.
        (rule("enabled = false"), "", none, a.clone(), Ctx { now: 0, paused: true, cause_rule: None }, vec!["RuleDisabled".into()]),
        (rule(""), "", none, a.clone(), Ctx { now: 0, paused: true, cause_rule: None }, vec!["GlobalPause".into()]),
        (rule(""), "", |s| s.snooze("lecture", t(100)), a.clone(), at(50), vec!["RuleSnoozed { until: Timestamp(100) }".into()]),
        (rule(""), "", |s| s.snooze("lecture", t(100)), a.clone(), at(100), vec!["fire".into()]),
        (rule(""), "", |s| s.restore_rule("lecture", RuleRuntime { circuit_open_until: Some(t(100)), ..Default::default() }), a.clone(), at(0), vec!["CircuitOpen { until: Timestamp(100) }".into()]),
        // 3. Permission: the rule matches (its where allows any pdf) but the agent cannot see it.
        (
            r#"[[rule]]
name = "lecture"
on = "fs.file.*"
where = { path = "/home/me/**/*.pdf" }
run = { agent = "study", prompt = "x" }"#.into(),
            "", none, outside.clone(), at(0),
            vec!["PermissionDenied { needed: \"fs.file.created:/home/me/Secret/a.pdf\" }".into()],
        ),
        // 4. Loop guard.
        (rule(""), "", none, a.clone(), Ctx { now: 0, paused: false, cause_rule: Some("lecture") }, vec![format!("LoopSelfTrigger {{ run: {run_id:?} }}")]),
        (rule(""), "", none, a.clone(), Ctx { now: 0, paused: false, cause_rule: Some("other") }, vec!["fire".into()]),
        (rule(""), "", none, Event { chain_depth: 2, ..a.clone() }, at(0), vec!["fire".into()]),
        (rule(""), "", none, Event { chain_depth: 3, ..a.clone() }, at(0), vec!["LoopChainDepth { depth: 4, max: 3 }".into()]),
        (rule(""), "[limits]\nmax_chain_depth = 1", none, Event { chain_depth: 1, ..a.clone() }, at(0), vec!["LoopChainDepth { depth: 2, max: 1 }".into()]),
        // 5. Debounce.
        (rule("debounce = \"10s\""), "", none, a.clone(), at(5), vec!["debounced until 10005".into()]),
        // 6. Cooldown and rate limit, from fire history.
        (rule("cooldown = \"30s\""), "", |s| s.restore_rule("lecture", RuleRuntime { fires: FireHistory::from_times(vec![t(0)], 10), ..Default::default() }), a.clone(), at(10_000), vec!["Cooldown { until: Timestamp(30000) }".into()]),
        (rule("cooldown = \"30s\""), "", |s| s.restore_rule("lecture", RuleRuntime { fires: FireHistory::from_times(vec![t(0)], 10), ..Default::default() }), a.clone(), at(30_000), vec!["fire".into()]),
        (rule("rate_limit = \"2/1m\""), "", |s| s.restore_rule("lecture", RuleRuntime { fires: FireHistory::from_times(vec![t(0), t(1)], 10), ..Default::default() }), a.clone(), at(2), vec!["RateLimited { until: Timestamp(60000) }".into()]),
        // 7. Circuit breaker trips on the fire that would exceed it.
        (rule(""), "[limits]\ncircuit_breaker = { max_fires = 2, window = \"1m\", open_for = \"1h\" }", |s| s.restore_rule("lecture", RuleRuntime { fires: FireHistory::from_times(vec![t(0), t(1)], 10), ..Default::default() }), a.clone(), at(2), vec!["CircuitOpen { until: Timestamp(3600002) }".into()]),
        // 8. Agent backlog.
        (
            "[[rule]]\nname = \"solo\"\non = \"fs.*\"\nrun = { agent = \"solo\", prompt = \"x\" }".into(),
            "", |s| s.set_agent_backlog("solo", 1), a.clone(), at(0), vec!["AgentBusy { backlog: 1, max: 1 }".into()],
        ),
        // Supervised runs wait for a human, not for the agent.
        (
            "[[rule]]\nname = \"solo\"\non = \"fs.*\"\nsupervised = true\nrun = { agent = \"solo\", prompt = \"x\" }".into(),
            "", |s| s.set_agent_backlog("solo", 1), a.clone(), at(0), vec!["await_approval".into()],
        ),
        // 10. Command agents may have no prompt.
        ("[[rule]]\nname = \"n\"\non = \"*\"\nrun = { agent = \"notify\" }".into(), "", none, a.clone(), at(0), vec!["fire(no prompt)".into()]),
    ];
    for (rules, limits, setup, event, ctx, want) in cases {
        let r = routing(&rules, limits);
        let mut state = RouterState::new(r.limits());
        setup(&mut state);
        let got: Vec<String> = run_route(&event, RouteMode::AllRules, &r, &mut state, ctx)
            .iter()
            .map(|d| label(&d.outcome))
            .collect();
        assert_eq!(got, want, "rules:\n{rules}\nlimits: {limits}");
    }
}

#[test]
fn decisions_follow_config_order_and_count_backlog() {
    let rules = r#"
[[rule]]
name = "first"
on = "fs.*"
run = { agent = "solo", prompt = "one" }

[[rule]]
name = "second"
on = "fs.file.created"
run = { agent = "solo", prompt = "two" }

[[rule]]
name = "third"
on = "fs.*"
run = { agent = "study", prompt = "Read {{subject}}" }
"#;
    let r = routing(rules, "");
    let mut state = RouterState::new(r.limits());
    let d = run_route(&pdf(1, "a.pdf"), RouteMode::AllRules, &r, &mut state, at(0));
    let got: Vec<(&str, String, u32)> =
        d.iter().map(|d| (d.rule.as_str(), label(&d.outcome), d.chain_depth)).collect();
    assert_eq!(
        got,
        vec![
            ("first", "fire".into(), 1),
            // The first fire filled solo's only slot within this same call.
            ("second", "AgentBusy { backlog: 1, max: 1 }".into(), 1),
            ("third", "fire".into(), 1),
        ]
    );
    let Outcome::Fire { prompt: Some(p) } = &d[2].outcome else { panic!() };
    assert!(p.text.starts_with("Read /home/me/University/a.pdf\n"));
    assert_eq!(state.agent_backlog("solo"), 1);
    assert_eq!(state.rule("first").unwrap().fires.len(), 1);
    assert!(state.rule("second").unwrap().fires.is_empty());

    let only = ["third".to_owned()];
    let d = run_route(&pdf(2, "b.pdf"), RouteMode::Only(&only), &r, &mut state, at(1));
    assert_eq!(d.iter().map(|d| d.rule.as_str()).collect::<Vec<_>>(), ["third"]);
}

#[test]
fn debounce_fires_the_latest_event_once() {
    let r = routing(&format!("{RULE}debounce = \"10s\"\n"), "");
    let mut state = RouterState::new(r.limits());
    let (a, b) = (pdf(1, "a.pdf"), pdf(2, "b.pdf"));
    assert_eq!(
        label(&run_route(&a, RouteMode::AllRules, &r, &mut state, at(0))[0].outcome),
        "debounced until 10000"
    );
    assert_eq!(
        label(&run_route(&b, RouteMode::AllRules, &r, &mut state, at(5_000))[0].outcome),
        "debounced until 15000"
    );
    assert_eq!(state.next_deadline(), Some(t(15_000)));
    assert!(state.take_due(t(14_999)).is_empty());
    let due = state.take_due(t(15_000));
    assert_eq!(due, vec![DueDebounce { rule: "lecture".into(), event: b.id }]);
    assert_eq!(state.next_deadline(), None);
    let d = run_route(&b, RouteMode::DebounceElapsed("lecture"), &r, &mut state, at(15_000));
    assert_eq!(label(&d[0].outcome), "fire");
    // DebounceElapsed evaluates only that rule.
    let d = run_route(&b, RouteMode::DebounceElapsed("other"), &r, &mut state, at(15_000));
    assert!(d.is_empty());
}

#[test]
fn debounce_by_subject_keeps_one_timer_per_subject() {
    let r = routing(&format!("{RULE}debounce = \"10s\"\ndebounce_by = \"subject\"\n"), "");
    let mut state = RouterState::new(r.limits());
    for (i, name) in ["a.pdf", "b.pdf", "a.pdf"].iter().enumerate() {
        run_route(
            &pdf(i as u64 + 1, name),
            RouteMode::AllRules,
            &r,
            &mut state,
            at(i as i64 * 1000),
        );
    }
    let due: Vec<EventId> = state.take_due(t(20_000)).into_iter().map(|d| d.event).collect();
    assert_eq!(due, vec![pdf(2, "b.pdf").id, pdf(3, "a.pdf").id]);
}

#[test]
fn elapsed_debounce_waits_out_the_cooldown_instead_of_dropping() {
    let r = routing(&format!("{RULE}debounce = \"10s\"\ncooldown = \"30s\"\n"), "");
    let mut state = RouterState::new(r.limits());
    let (a, b) = (pdf(1, "a.pdf"), pdf(2, "b.pdf"));
    run_route(&a, RouteMode::AllRules, &r, &mut state, at(0));
    state.take_due(t(10_000));
    assert_eq!(
        label(
            &run_route(&a, RouteMode::DebounceElapsed("lecture"), &r, &mut state, at(10_000))[0]
                .outcome
        ),
        "fire"
    );
    run_route(&b, RouteMode::AllRules, &r, &mut state, at(12_000));
    assert_eq!(state.take_due(t(22_000)).len(), 1);
    let d = run_route(&b, RouteMode::DebounceElapsed("lecture"), &r, &mut state, at(22_000));
    assert_eq!(label(&d[0].outcome), "debounced until 40000", "re-armed until the cooldown ends");
    assert_eq!(state.next_deadline(), Some(t(40_000)));
    state.take_due(t(40_000));
    assert_eq!(
        label(
            &run_route(&b, RouteMode::DebounceElapsed("lecture"), &r, &mut state, at(40_000))[0]
                .outcome
        ),
        "fire"
    );
}

#[test]
fn a_full_debouncer_degrades_to_no_debounce() {
    let r = routing(
        &format!("{RULE}debounce = \"10s\"\ndebounce_by = \"subject\"\n"),
        "[limits]\ndebounce_capacity = 1",
    );
    let mut state = RouterState::new(r.limits());
    let first = run_route(&pdf(1, "a.pdf"), RouteMode::AllRules, &r, &mut state, at(0));
    let second = run_route(&pdf(2, "b.pdf"), RouteMode::AllRules, &r, &mut state, at(0));
    assert_eq!(label(&first[0].outcome), "debounced until 10000");
    assert_eq!(label(&second[0].outcome), "fire");
}

#[test]
fn dry_runs_do_not_touch_the_real_state() {
    let r = routing(&format!("{RULE}cooldown = \"30s\"\n"), "");
    let mut state = RouterState::new(r.limits());
    let mut copy = state.clone();
    assert_eq!(
        label(&run_route(&pdf(1, "a.pdf"), RouteMode::AllRules, &r, &mut copy, at(0))[0].outcome),
        "fire"
    );
    assert!(state.rule("lecture").is_none());
    assert_eq!(
        label(&run_route(&pdf(1, "a.pdf"), RouteMode::AllRules, &r, &mut state, at(0))[0].outcome),
        "fire"
    );
}

#[test]
fn breaker_stays_open_and_unsnooze_closes_it() {
    let r = routing(
        RULE,
        "[limits]\ncircuit_breaker = { max_fires = 1, window = \"1m\", open_for = \"1h\" }",
    );
    let mut state = RouterState::new(r.limits());
    let a = pdf(1, "a.pdf");
    let labels: Vec<String> = (0..3)
        .map(|i| label(&run_route(&a, RouteMode::AllRules, &r, &mut state, at(i))[0].outcome))
        .collect();
    assert_eq!(
        labels,
        [
            "fire",
            "CircuitOpen { until: Timestamp(3600001) }",
            "CircuitOpen { until: Timestamp(3600001) }"
        ]
    );
    state.unsnooze("lecture");
    assert_eq!(state.rule("lecture").unwrap().circuit_open_until, None);
    // Still within the window: the breaker trips again on the next fire.
    assert!(
        label(&run_route(&a, RouteMode::AllRules, &r, &mut state, at(3))[0].outcome)
            .starts_with("CircuitOpen")
    );
    assert_eq!(
        label(&run_route(&a, RouteMode::AllRules, &r, &mut state, at(3_600_004))[0].outcome),
        "fire"
    );
}

#[test]
fn retain_configured_drops_removed_rules() {
    let r = routing(&format!("{RULE}debounce = \"10s\"\n"), "");
    let mut state = RouterState::new(r.limits());
    run_route(&pdf(1, "a.pdf"), RouteMode::AllRules, &r, &mut state, at(0));
    state.set_agent_backlog("gone", 3);
    let empty = routing("", "");
    state.retain_configured(&empty);
    assert!(state.rule("lecture").is_none());
    assert_eq!(state.next_deadline(), None);
    assert_eq!(state.agent_backlog("gone"), 0);
    assert!(r.agent_permits("study", &pdf(1, "a.pdf")));
    assert!(!r.agent_permits("nobody", &pdf(1, "a.pdf")));
}
