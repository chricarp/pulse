//! The `codex` adapter: Codex CLI's `exec` mode with JSONL output.
//!
//! ```text
//! codex exec --json --sandbox=<sandbox, default read-only>
//!            --config=approval_policy="<approval_policy, default never>"
//!            [--model=<model>] <extra_args>…
//! stdin: the rendered prompt (no positional prompt, so codex reads stdin)
//! ```
//!
//! **Permissions.** Both settings are always passed, so the user's interactive defaults in
//! `~/.codex/config.toml` (which may be `danger-full-access`) never apply to an event-triggered
//! run. `read-only` lets the agent read and run read-only commands; anything that writes fails
//! inside the sandbox and the model is told. `never` means Codex never waits for an approval
//! nobody can give: a command that would need one fails and the failure is returned to the
//! model, and shows up here as a failed `tool_result`. Codex's own git-repository check is left
//! on; pass `--skip-git-repo-check` in `extra_args` to run outside a repository.
//!
//! **Output mapping** (`type` of each JSONL event, `item.type` for items):
//!
//! | Native | Output |
//! |---|---|
//! | `thread.started` | `system` (`data.thread_id`) |
//! | `turn.started`, `item.updated` (except plans) | dropped |
//! | `item.started` command_execution / mcp_tool_call | `tool_use` |
//! | `item.completed` command_execution / mcp_tool_call | `tool_result` (`tool_use` first if its start was not seen) |
//! | `item.completed` file_change / web_search | `tool_use` |
//! | `item.*` todo_list | `system` (the plan, with checkmarks) |
//! | `item.completed` reasoning / agent_message / error | `thinking` / `assistant` / `error` |
//! | `turn.completed` | `result`: the last agent message; `data.usage` |
//! | `turn.failed`, `error` | `error` |
//! | anything else, non-JSON, stderr | `stdout` / `stderr`, unchanged |

use std::collections::HashSet;

use pulse_contracts::config::{AdapterSettings, AgentConfig, AgentKind};
use pulse_contracts::{OutputKind, RunOutput};
use serde_json::Value;

use crate::json::{PREVIEW_CHARS, SUMMARY_CHARS, cut, one_line, output, pick, raw, str_at};
use crate::{AdapterError, AgentAdapter, Invocation, OutputParser, RunRequest, Stream, run_env};

pub const DEFAULT_SANDBOX: &str = "read-only";
pub const DEFAULT_APPROVAL_POLICY: &str = "never";

#[derive(Debug)]
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn invocation(
        &self,
        agent: &AgentConfig,
        req: &RunRequest,
    ) -> Result<Invocation, AdapterError> {
        let AdapterSettings::Codex(s) = &agent.adapter else {
            return Err(AdapterError::WrongAdapter {
                agent: agent.name.clone(),
                configured: agent.kind(),
            });
        };
        let prompt = req
            .prompt
            .as_ref()
            .ok_or_else(|| AdapterError::Invalid("a codex run needs a prompt".into()))?;
        let approval = s.approval_policy.as_deref().unwrap_or(DEFAULT_APPROVAL_POLICY);
        // The value is spliced into a TOML expression; anything but a bare word could change
        // which key it sets.
        if approval.is_empty() || !approval.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(AdapterError::Invalid(format!(
                "approval_policy {approval:?} is not a policy name (like \"never\" or \"on-request\")"
            )));
        }
        let mut args: Vec<String> = vec![
            "exec".into(),
            "--json".into(),
            format!("--sandbox={}", s.sandbox.as_deref().unwrap_or(DEFAULT_SANDBOX)),
            format!("--config=approval_policy=\"{approval}\""),
        ];
        if let Some(model) = &s.model {
            args.push(format!("--model={model}"));
        }
        args.extend(s.extra_args.iter().cloned());
        let mut env: Vec<(String, String)> =
            agent.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        env.extend(run_env(req));
        Ok(Invocation {
            program: s.program.clone().unwrap_or_else(|| "codex".to_owned()),
            args,
            cwd: req.cwd.clone(),
            env,
            stdin: Some(prompt.text.clone()),
        })
    }

    fn parser(&self) -> Box<dyn OutputParser> {
        Box::new(CodexParser::default())
    }
}

/// Bound on remembered item ids; cleared when full (a completion then repeats its `tool_use`).
const STARTED_MAX: usize = 256;

#[derive(Debug, Default)]
pub struct CodexParser {
    /// Tool items whose start was reported, so their completion is not reported as a new call.
    started: HashSet<String>,
    last_message: Option<String>,
}

impl OutputParser for CodexParser {
    fn line(&mut self, stream: Stream, line: &str, out: &mut Vec<RunOutput>) {
        if stream == Stream::Stderr {
            out.push(raw(stream, line));
            return;
        }
        if line.trim().is_empty() {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            out.push(raw(stream, line));
            return;
        };
        let known = match str_at(&v, "type") {
            Some("thread.started") => {
                let id = str_at(&v, "thread_id").unwrap_or("?");
                out.push(output(
                    OutputKind::System,
                    format!("Codex session {id} started"),
                    v.clone(),
                ));
                true
            }
            Some("turn.started") => true,
            Some("turn.completed") => {
                let text = self.last_message.take().unwrap_or_else(|| "Turn completed".to_owned());
                out.push(output(OutputKind::Result, text, pick(&v, &["usage"])));
                true
            }
            Some("turn.failed") => {
                let message = v.pointer("/error/message").and_then(Value::as_str);
                out.push(output(
                    OutputKind::Error,
                    format!("Turn failed: {}", message.unwrap_or("unknown error")),
                    v.get("error").cloned().unwrap_or(Value::Null),
                ));
                true
            }
            Some("error") => {
                out.push(output(
                    OutputKind::Error,
                    str_at(&v, "message").unwrap_or("error"),
                    v.clone(),
                ));
                true
            }
            Some(phase @ ("item.started" | "item.updated" | "item.completed")) => {
                match v.get("item").filter(|i| i.is_object()) {
                    Some(item) => {
                        self.item(phase, item, out);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        };
        if !known {
            out.push(raw(stream, line));
        }
    }

    fn finish(&mut self, _: &mut Vec<RunOutput>) {}
}

impl CodexParser {
    fn item(&mut self, phase: &str, item: &Value, out: &mut Vec<RunOutput>) {
        let id = str_at(item, "id").unwrap_or("");
        let completed = phase == "item.completed";
        match str_at(item, "type") {
            Some(kind @ ("command_execution" | "mcp_tool_call")) => {
                let summary = if kind == "command_execution" {
                    format!("$ {}", str_at(item, "command").unwrap_or(""))
                } else {
                    format!(
                        "{}.{} {}",
                        str_at(item, "server").unwrap_or("mcp"),
                        str_at(item, "tool").unwrap_or("tool"),
                        item.get("arguments").map(Value::to_string).unwrap_or_default()
                    )
                };
                let summary = one_line(&summary, SUMMARY_CHARS);
                match phase {
                    "item.started" => {
                        self.remember(id);
                        out.push(output(OutputKind::ToolUse, summary, item.clone()));
                    }
                    "item.completed" => {
                        if !self.started.remove(id) {
                            out.push(output(OutputKind::ToolUse, summary, item.clone()));
                        }
                        out.push(output(
                            OutputKind::ToolResult,
                            tool_result_text(kind, item),
                            item.clone(),
                        ));
                    }
                    _ => {}
                }
            }
            Some("file_change") if completed => {
                let changes = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .map(|cs| {
                        cs.iter()
                            .map(|c| {
                                format!(
                                    "{} {}",
                                    str_at(c, "kind").unwrap_or("change"),
                                    str_at(c, "path").unwrap_or("?")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let status = status_suffix(item);
                out.push(output(
                    OutputKind::ToolUse,
                    one_line(&format!("patch{status}: {changes}"), SUMMARY_CHARS),
                    item.clone(),
                ));
            }
            Some("web_search") if completed => {
                let query = str_at(item, "query").unwrap_or("");
                out.push(output(
                    OutputKind::ToolUse,
                    one_line(&format!("web search {query}"), SUMMARY_CHARS),
                    item.clone(),
                ));
            }
            Some("todo_list") => {
                let items =
                    item.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                let plan = items
                    .iter()
                    .map(|t| {
                        let done = t.get("completed").and_then(Value::as_bool).unwrap_or(false);
                        format!(
                            "[{}] {}",
                            if done { "x" } else { " " },
                            str_at(t, "text").unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                out.push(output(OutputKind::System, format!("Plan:\n{plan}"), item.clone()));
            }
            Some("reasoning") if completed => {
                let text = str_at(item, "text").unwrap_or("");
                if !text.is_empty() {
                    out.push(output(OutputKind::Thinking, text, pick(item, &["id"])));
                }
            }
            Some("agent_message") if completed => {
                let text = str_at(item, "text").unwrap_or("");
                self.last_message = Some(text.to_owned());
                out.push(output(OutputKind::Assistant, text, pick(item, &["id"])));
            }
            Some("error") if completed => {
                out.push(output(
                    OutputKind::Error,
                    str_at(item, "message").unwrap_or("error"),
                    item.clone(),
                ));
            }
            Some(_) if !completed => {}
            other => {
                out.push(output(
                    OutputKind::System,
                    format!("[{}]", other.unwrap_or("item")),
                    item.clone(),
                ));
            }
        }
    }

    fn remember(&mut self, id: &str) {
        if self.started.len() >= STARTED_MAX {
            self.started.clear();
        }
        self.started.insert(id.to_owned());
    }
}

fn status_suffix(item: &Value) -> String {
    match str_at(item, "status") {
        Some("completed") | None => String::new(),
        Some(other) => format!(" ({other})"),
    }
}

fn tool_result_text(kind: &str, item: &Value) -> String {
    let status = status_suffix(item);
    if kind == "command_execution" {
        let code = item.get("exit_code").and_then(Value::as_i64);
        let head = match code {
            Some(c) => format!("exit {c}{status}"),
            None => format!("ended{status}"),
        };
        let output = str_at(item, "aggregated_output").unwrap_or("").trim_end();
        if output.is_empty() { head } else { format!("{head}\n{}", cut(output, PREVIEW_CHARS)) }
    } else {
        let body = match item.get("error").filter(|e| !e.is_null()) {
            Some(e) => format!(
                "error: {}",
                str_at(e, "message").map(str::to_owned).unwrap_or_else(|| e.to_string())
            ),
            None => item.get("result").map(Value::to_string).unwrap_or_default(),
        };
        let text = format!("{}{status} {}", str_at(item, "tool").unwrap_or("tool"), body);
        cut(text.trim_end(), PREVIEW_CHARS)
    }
}

#[cfg(test)]
mod tests;
