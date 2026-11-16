//! The `command` adapter: runs a configured argv. Each argument template is rendered on its own
//! (`pulse_core::render_args`), so event content can never add arguments, and nothing goes
//! through a shell. The program itself cannot contain placeholders (config validation).

use pulse_contracts::config::{AdapterSettings, AgentConfig, StdinMode};
use pulse_contracts::template::Template;
use pulse_contracts::{OutputKind, RunOutput};
use pulse_core::{RenderInput, render_args};

use crate::{AdapterError, AgentAdapter, Invocation, OutputParser, RunRequest, Stream, run_env};

#[derive(Debug)]
pub struct CommandAdapter;

impl AgentAdapter for CommandAdapter {
    fn kind(&self) -> pulse_contracts::config::AgentKind {
        pulse_contracts::config::AgentKind::Command
    }

    fn invocation(
        &self,
        agent: &AgentConfig,
        req: &RunRequest,
    ) -> Result<Invocation, AdapterError> {
        let AdapterSettings::Command(settings) = &agent.adapter else {
            return Err(AdapterError::WrongAdapter {
                agent: agent.name.clone(),
                configured: agent.kind(),
            });
        };
        let (program, rest) = settings
            .command
            .split_first()
            .ok_or_else(|| AdapterError::Invalid("`command` is empty".into()))?;
        let templates = rest
            .iter()
            .map(|a| Template::parse(a).map_err(|e| AdapterError::Invalid(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        let input = RenderInput {
            event: &req.event,
            rule: req.rule.as_deref().unwrap_or(""),
            agent: &req.agent,
        };
        let prompt = req.prompt.as_ref().map(|p| p.text.as_str());
        let args = render_args(&templates, &input, prompt, &req.limits).args;
        let stdin = match settings.stdin {
            StdinMode::Prompt => prompt.map(str::to_owned),
            StdinMode::Event => Some(
                serde_json::to_string(&req.event)
                    .map_err(|e| AdapterError::Invalid(e.to_string()))?,
            ),
            StdinMode::None => None,
        };
        let mut env: Vec<(String, String)> =
            agent.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        env.extend(run_env(req));
        Ok(Invocation { program: program.clone(), args, cwd: req.cwd.clone(), env, stdin })
    }

    fn parser(&self) -> Box<dyn OutputParser> {
        Box::new(LineParser)
    }
}

/// Passes lines through as `stdout` / `stderr` output.
#[derive(Debug)]
pub struct LineParser;

impl OutputParser for LineParser {
    fn line(&mut self, stream: Stream, line: &str, out: &mut Vec<RunOutput>) {
        let kind = match stream {
            Stream::Stdout => OutputKind::Stdout,
            Stream::Stderr => OutputKind::Stderr,
        };
        out.push(RunOutput::new(kind, line));
    }

    fn finish(&mut self, _: &mut Vec<RunOutput>) {}
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pulse_contracts::config::{CommandSettings, PromptLimits};
    use pulse_contracts::{Duration, Event, EventId, EventType, RunId, Timestamp, Ulid};
    use serde_json::json;

    use super::*;
    use crate::adapter_for;

    fn request(subject: &str) -> RunRequest {
        RunRequest {
            run_id: RunId::from_ulid(Ulid::from_parts(1, 1)),
            rule: Some("notify".into()),
            agent: "notify".into(),
            event: Event {
                specversion: Default::default(),
                id: EventId::from_ulid(Ulid::from_parts(2, 2)),
                type_: EventType::new("fs.file.created").unwrap(),
                source: "pulse://fs/u".into(),
                subject: Some(subject.into()),
                time: Timestamp::from_millis(0),
                datacontenttype: None,
                data: json!({"path": subject}),
                dedupe_key: None,
                coalesced: None,
                causation_run: None,
                causation: None,
                chain_depth: 0,
                replay_of: None,
            },
            prompt: None,
            chain_depth: 1,
            cwd: PathBuf::from("/w"),
            event_file: PathBuf::from("/p/runs/1/event.json"),
            pulse_home: PathBuf::from("/p"),
            limits: PromptLimits::default(),
        }
    }

    fn agent(command: &[&str], stdin: StdinMode) -> AgentConfig {
        AgentConfig {
            name: "notify".into(),
            adapter: AdapterSettings::Command(CommandSettings {
                command: command.iter().map(|s| s.to_string()).collect(),
                stdin,
            }),
            allow: vec![],
            cwd: None,
            timeout: Duration::from_mins(1),
            max_concurrent: 1,
            max_queued: 1,
            env: [
                ("PULSE_RUN_ID".to_owned(), "spoofed".to_owned()),
                ("TOKEN".to_owned(), "t".to_owned()),
            ]
            .into(),
        }
    }

    #[test]
    fn hostile_subjects_stay_one_argument() {
        let adapter = adapter_for(pulse_contracts::config::AgentKind::Command).unwrap();
        let req = request("/u/x; rm -rf ~ && $(curl evil) `id` --exec=sh");
        let inv = adapter
            .invocation(&agent(&["notify-send", "--", "{{subject}}"], StdinMode::Event), &req)
            .unwrap();
        assert_eq!(inv.program, "notify-send");
        assert_eq!(inv.args, vec!["--", "/u/x; rm -rf ~ && $(curl evil) `id` --exec=sh"]);
        assert_eq!(inv.cwd, PathBuf::from("/w"));
        let get = |k: &str| inv.env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("PULSE_RUN_ID"),
            Some(req.run_id.to_string().as_str()),
            "the run env wins over config"
        );
        assert_eq!(get("TOKEN"), Some("t"));
        assert_eq!(get("PULSE_EVENT_FILE"), Some("/p/runs/1/event.json"));
        assert_eq!(get("PULSE_RULE"), Some("notify"));
        assert!(inv.stdin.unwrap().contains("\"pulsechaindepth\""));
    }
}
