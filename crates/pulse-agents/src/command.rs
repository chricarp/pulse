//! The `command` adapter: runs a configured argv. Each argument template is rendered on its own
//! (`pulse_core::render_args`), so event content can never add arguments, and nothing goes
//! through a shell. The program itself cannot contain placeholders (config validation).
//!
//! One check on top: an argument whose template *starts* with a placeholder must not render to
//! something starting with `-` unless an earlier argument is exactly `--`. Otherwise a file
//! named `--exec=sh` would reach the program as an option. The run fails before spawning, with
//! a message saying how to fix the template. Values after literal text (`--title={{subject}}`)
//! are not affected: the option is the user's, the value cannot become another one.

use pulse_contracts::config::{AdapterSettings, AgentConfig, StdinMode};
use pulse_contracts::template::{Template, TemplatePart};
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
        for (i, (template, arg)) in templates.iter().zip(&args).enumerate() {
            if rest[i] == "--" {
                break;
            }
            if arg.starts_with('-') && matches!(template.parts.first(), Some(TemplatePart::Var(_)))
            {
                return Err(AdapterError::Invalid(format!(
                    "command[{}] ({:?}) rendered to a value starting with '-', which the program could read as an option; put \"--\" before event-derived arguments, or read the event from PULSE_EVENT_FILE",
                    i + 1,
                    rest[i]
                )));
            }
        }
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
    use pulse_contracts::config::{AgentKind, CommandSettings};

    use super::*;
    use crate::adapter_for;
    use crate::test_support::{HOSTILE, agent, env, request};

    fn command(argv: &[&str], stdin: StdinMode) -> AgentConfig {
        agent(AdapterSettings::Command(CommandSettings {
            command: argv.iter().map(|s| s.to_string()).collect(),
            stdin,
        }))
    }

    #[test]
    fn hostile_subjects_stay_one_argument() {
        let adapter = adapter_for(AgentKind::Command).unwrap();
        for subject in HOSTILE {
            let req = request(subject);
            let inv = adapter
                .invocation(
                    &command(&["notify-send", "--", "{{subject}}", "x{{name}}"], StdinMode::Event),
                    &req,
                )
                .unwrap();
            assert_eq!(inv.program, "notify-send");
            assert_eq!(inv.args, vec!["--".to_owned(), subject.to_owned(), format!("x{subject}")]);
            assert_eq!(inv.cwd, std::path::PathBuf::from("/w"));
            assert_eq!(
                env(&inv.env, "PULSE_RUN_ID"),
                Some(req.run_id.to_string().as_str()),
                "the run env wins over config"
            );
            assert_eq!(env(&inv.env, "TOKEN"), Some("t"));
            assert_eq!(env(&inv.env, "PULSE_EVENT_FILE"), Some("/p/runs/1/event.json"));
            assert_eq!(env(&inv.env, "PULSE_RULE"), Some("notify"));
            assert!(inv.stdin.unwrap().contains("\"pulsechaindepth\""));
        }
    }

    #[test]
    fn option_looking_values_need_a_double_dash() {
        let adapter = adapter_for(AgentKind::Command).unwrap();
        let req = request("--exec=sh");
        // (argv, accepted)
        let cases: [(&[&str], bool); 5] = [
            (&["lp", "{{name}}"], false),
            (&["lp", "-o", "x", "{{subject}}"], false),
            (&["lp", "--", "{{name}}"], true),
            (&["lp", "--title={{name}}"], true),
            (&["lp", "./{{name}}"], true),
        ];
        for (argv, accepted) in cases {
            let result = adapter.invocation(&command(argv, StdinMode::None), &req);
            assert_eq!(result.is_ok(), accepted, "{argv:?}: {result:?}");
        }
        let err =
            adapter.invocation(&command(&["lp", "{{name}}"], StdinMode::None), &req).unwrap_err();
        assert!(err.to_string().contains("command[1]"), "{err}");
    }

    #[test]
    fn stdin_modes() {
        let adapter = adapter_for(AgentKind::Command).unwrap();
        let req = request("/u/a.pdf");
        let stdin = |mode| adapter.invocation(&command(&["cat"], mode), &req).unwrap().stdin;
        assert_eq!(stdin(StdinMode::Prompt).as_deref(), Some("Summarize /u/a.pdf"));
        assert_eq!(stdin(StdinMode::None), None);
        let mut no_prompt = request("/u/a.pdf");
        no_prompt.prompt = None;
        let inv = adapter.invocation(&command(&["cat"], StdinMode::Prompt), &no_prompt).unwrap();
        assert_eq!(inv.stdin, None);
    }
}
