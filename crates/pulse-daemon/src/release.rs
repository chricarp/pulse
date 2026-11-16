//! What a run executes, and whether it may still be released.
//!
//! A run captures the rule and agent when it is created (its snapshot), and its invocation
//! (argv and working directory) is rendered then, so a supervised run's approval shows what
//! will execute. Config can change before the run starts: an agent deleted or edited, a project
//! untrusted or changed by a `git pull`. **A run never starts with an agent definition that is
//! not the current one.** At each point where a run is released (approval, `queued → running`,
//! rerun) its snapshot is compared with the current config; a run that no longer matches ends
//! without starting ([`stale_reason`]). And what starts is what was shown: the invocation is
//! rendered again from the snapshot and must equal the stored one.

use std::path::PathBuf;

use pulse_agents::{Invocation, RunRequest};
use pulse_contracts::config::{AgentConfig, Config};
use pulse_contracts::pattern::expand_path;
use pulse_contracts::run::RunSnapshot;
use pulse_contracts::{Event, RenderedPrompt, RunId};

use crate::shared::Shared;

/// Why a run with this snapshot must not start under `config`, if it must not.
///
/// - The agent must exist and be defined exactly as in the snapshot: a removed agent, an
///   edited one, or one from a project that was untrusted or whose file changed (it is then
///   not in the config at all) stops the run.
/// - When the run has a rule, the rule must exist, not be archived, and its `run` (the agent
///   and the prompt template, which decide what executes) must be as in the snapshot. Other
///   rule fields (`where`, limits, `enabled`, `supervised`) decide whether an event fires, not
///   what a run that already exists executes.
pub(crate) fn stale_reason(config: &Config, agent: &str, snapshot: &RunSnapshot) -> Option<String> {
    let current = config.agent(agent).map(|a| &a.config);
    if current != Some(&snapshot.agent) {
        return Some(
            "the agent was removed or changed after this run was created; replay the event to run it with the current definition"
                .to_owned(),
        );
    }
    if let Some(rule) = &snapshot.rule {
        let same =
            config.rule(&rule.name).is_some_and(|r| !r.config.archived && r.config.run == rule.run);
        if !same {
            return Some(
                "the rule was removed, archived or its prompt changed after this run was created; replay the event to run it with the current definition"
                    .to_owned(),
            );
        }
    }
    None
}

/// The parts of a run that decide its invocation.
pub(crate) struct RunParts<'a> {
    pub id: RunId,
    pub rule: Option<&'a str>,
    /// The agent's full name (`project/agent` for a project agent).
    pub agent_name: &'a str,
    pub agent: &'a AgentConfig,
    pub prompt: Option<&'a str>,
    pub chain_depth: u32,
}

/// The invocation for a run, from its own parts and the config it was created under (which
/// resolves a project agent's relative `cwd`). No I/O: the event file and run directory are
/// paths here; the worker creates them before it starts the process.
pub(crate) fn render(
    shared: &Shared,
    config: &Config,
    run: &RunParts<'_>,
    event: &Event,
) -> Result<Invocation, String> {
    let adapter = (shared.adapters)(run.agent.kind())
        .ok_or_else(|| format!("agent {:?} uses an adapter Pulse does not run", run.agent.name))?;
    let dir = shared.home.run_dir(run.id);
    let base = config.agent(run.agent_name).and_then(|a| a.origin.base_dir().map(PathBuf::from));
    let cwd = match &run.agent.cwd {
        Some(raw) => PathBuf::from(
            expand_path(raw, &shared.user_home, base.as_deref())
                .map_err(|e| format!("agent cwd: {e}"))?,
        ),
        None => dir.clone(),
    };
    let request = RunRequest {
        run_id: run.id,
        rule: run.rule.map(str::to_owned),
        agent: run.agent_name.to_owned(),
        event: event.clone(),
        // Only the text is stored; adapters send the text.
        prompt: run.prompt.map(|text| RenderedPrompt {
            text: text.to_owned(),
            delimiter: String::new(),
            truncated: false,
            missing: Vec::new(),
        }),
        chain_depth: run.chain_depth,
        cwd,
        event_file: dir.join("event.json"),
        pulse_home: shared.home.root().to_path_buf(),
        limits: config.limits.prompt.clone(),
    };
    adapter.invocation(run.agent, &request).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use pulse_contracts::config::{ConfigFile, ConfigOrigin, Sourced, resolve};

    use super::*;

    fn config(text: &str) -> Config {
        let file: ConfigFile = toml::from_str(text).unwrap();
        resolve(Sourced { origin: ConfigOrigin::User { path: "/p".into() }, config: file }, vec![])
            .unwrap()
    }

    const BASE: &str = r#"
[[agent]]
name = "fmt"
adapter = "command"
command = ["prettier", "--write", "--", "{{path}}"]
allow = ["fs.*"]

[[rule]]
name = "format"
on = "fs.file.modified"
run = { agent = "fmt" }
"#;

    #[test]
    fn a_run_starts_only_with_its_agent_and_rule_as_they_are_now() {
        let base = config(BASE);
        let snapshot = RunSnapshot {
            rule: Some(base.rule("format").unwrap().config.clone()),
            agent: base.agent("fmt").unwrap().config.clone(),
        };
        let cases = [
            (BASE.to_owned(), None),
            // Changes that do not affect what executes.
            (
                BASE.replace(
                    "on = \"fs.file.modified\"",
                    "on = \"fs.*\"\nsupervised = true\nenabled = false",
                ),
                None,
            ),
            (BASE.replace("\"prettier\"", "\"curl\""), Some("agent")),
            (BASE.replace("allow = [\"fs.*\"]", "allow = [\"fs.*\", \"git.*\"]"), Some("agent")),
            (
                BASE.replace(
                    "run = { agent = \"fmt\" }",
                    "run = { agent = \"fmt\", prompt = \"x\" }",
                ),
                Some("rule"),
            ),
            (
                BASE.replace(
                    "run = { agent = \"fmt\" }",
                    "run = { agent = \"fmt\" }\narchived = true",
                ),
                Some("rule"),
            ),
            (BASE.split("[[rule]]").next().unwrap().to_owned(), Some("rule")),
        ];
        for (text, want) in cases {
            let got = stale_reason(&config(&text), "fmt", &snapshot);
            match want {
                None => assert_eq!(got, None, "{text}"),
                Some(what) => {
                    assert!(got.is_some_and(|r| r.starts_with(&format!("the {what}"))), "{text}")
                }
            }
        }
        // An agent that is gone (deleted, or its project untrusted).
        let without = config("");
        assert!(stale_reason(&without, "fmt", &RunSnapshot { rule: None, ..snapshot }).is_some());
    }
}
