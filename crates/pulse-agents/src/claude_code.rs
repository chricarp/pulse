//! The `claude-code` adapter: Claude Code's print mode with streamed JSON output.
//!
//! ```text
//! claude --print --output-format=stream-json --verbose
//!        --permission-mode=<permission_mode, default dontAsk>
//!        --permission-prompts=none                (unless extra_args set a prompt handler)
//!        [--model=<model>] [--allowedTools=<tool>]… [--disallowedTools=<tool>]… <extra_args>…
//! stdin: the rendered prompt
//! ```
//!
//! Every value is attached to its flag with `=`, so a configured value can never be read as a
//! flag of its own, and the prompt (the only place event content appears) goes to stdin, never
//! into argv.
//!
//! **Permissions.** Nobody can answer a permission prompt in an unattended run. The default mode
//! is `dontAsk`: reads in the working directory and whatever the user's settings or
//! `allowed_tools` pre-approve are allowed, everything else is denied at once. The model is told,
//! the CLI emits a `permission_denied` event (an `error` output here), and the final result lists
//! the denials, which this parser turns into one more `error` output naming the tools. An
//! explicit `--permission-mode` also overrides a `defaultMode` from the user's own settings, so
//! an interactive `bypassPermissions` default does not leak into event-triggered runs.
//! `--permission-prompts=none` makes any other configured mode deny instead of waiting.
//!
//! **Output mapping** (`type`/`subtype` of each stream-json line):
//!
//! | Native | Output |
//! |---|---|
//! | `system/init` | `system`: version, model, permission mode; `data`: session id, cwd, tools |
//! | `system/permission_denied` | `error`: the tool that was denied |
//! | `system/thinking_tokens`, `stream_event`, allowed `rate_limit_event` | dropped (progress noise) |
//! | other `system`, limited `rate_limit_event` | `system` |
//! | `assistant` text / thinking / tool_use | `assistant` / `thinking` / `tool_use` (summarized input) |
//! | `assistant` with `error` (API errors) | `error` |
//! | `user` tool_result | `tool_result` (preview; `data.is_error`) |
//! | `result` | `result`: final text; `data`: cost, usage, turns, denials; then `error` for denials |
//! | anything else, non-JSON, stderr | `stdout` / `stderr`, unchanged |

use std::collections::HashMap;

use pulse_contracts::config::{AdapterSettings, AgentConfig, AgentKind};
use pulse_contracts::{OutputKind, RunOutput};
use serde_json::{Value, json};

use crate::json::{
    PREVIEW_CHARS, SUMMARY_CHARS, cut, one_line, output, pick, raw, str_at, tool_summary,
};
use crate::{AdapterError, AgentAdapter, Invocation, OutputParser, RunRequest, Stream, run_env};

/// The permission mode when the agent does not set one.
pub const DEFAULT_PERMISSION_MODE: &str = "dontAsk";

#[derive(Debug)]
pub struct ClaudeCodeAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn invocation(
        &self,
        agent: &AgentConfig,
        req: &RunRequest,
    ) -> Result<Invocation, AdapterError> {
        let AdapterSettings::ClaudeCode(s) = &agent.adapter else {
            return Err(AdapterError::WrongAdapter {
                agent: agent.name.clone(),
                configured: agent.kind(),
            });
        };
        let prompt = req
            .prompt
            .as_ref()
            .ok_or_else(|| AdapterError::Invalid("a claude-code run needs a prompt".into()))?;
        let mut args: Vec<String> =
            vec!["--print".into(), "--output-format=stream-json".into(), "--verbose".into()];
        let mode = s.permission_mode.as_deref().unwrap_or(DEFAULT_PERMISSION_MODE);
        args.push(format!("--permission-mode={mode}"));
        if !s.extra_args.iter().any(|a| a.starts_with("--permission-prompt")) {
            args.push("--permission-prompts=none".into());
        }
        if let Some(model) = &s.model {
            args.push(format!("--model={model}"));
        }
        args.extend(s.allowed_tools.iter().map(|t| format!("--allowedTools={t}")));
        args.extend(s.disallowed_tools.iter().map(|t| format!("--disallowedTools={t}")));
        args.extend(s.extra_args.iter().cloned());
        let mut env: Vec<(String, String)> =
            agent.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        env.extend(run_env(req));
        Ok(Invocation {
            program: s.program.clone().unwrap_or_else(|| "claude".to_owned()),
            args,
            cwd: req.cwd.clone(),
            env,
            stdin: Some(prompt.text.clone()),
        })
    }

    fn parser(&self) -> Box<dyn OutputParser> {
        Box::new(ClaudeCodeParser::default())
    }
}

/// Tool names by call id, so results can say which tool they belong to. Bounded: cleared when
/// full (a result then just lacks the name).
const TOOL_NAMES_MAX: usize = 256;

#[derive(Debug, Default)]
pub struct ClaudeCodeParser {
    tool_names: HashMap<String, String>,
}

impl OutputParser for ClaudeCodeParser {
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
        let before = out.len();
        let known = match str_at(&v, "type") {
            Some("system") => {
                self.system(&v, out);
                true
            }
            Some("assistant") => self.assistant(&v, out),
            Some("user") => self.user(&v, out),
            Some("result") => {
                result(&v, out);
                true
            }
            Some("rate_limit_event") => {
                rate_limit(&v, out);
                true
            }
            Some("stream_event") => true,
            _ => false,
        };
        if !known {
            out.truncate(before);
            out.push(raw(stream, line));
        }
    }

    fn finish(&mut self, _: &mut Vec<RunOutput>) {}
}

impl ClaudeCodeParser {
    fn system(&mut self, v: &Value, out: &mut Vec<RunOutput>) {
        match str_at(v, "subtype") {
            Some("init") => {
                let text = format!(
                    "Claude Code {} started: model {}, permission mode {}",
                    str_at(v, "claude_code_version").unwrap_or("?"),
                    str_at(v, "model").unwrap_or("?"),
                    str_at(v, "permissionMode").unwrap_or("?"),
                );
                let mut data = pick(
                    v,
                    &[
                        "session_id",
                        "model",
                        "permissionMode",
                        "cwd",
                        "claude_code_version",
                        "tools",
                    ],
                );
                if let Some(servers) = v.get("mcp_servers").and_then(Value::as_array) {
                    let servers: Vec<Value> =
                        servers.iter().map(|s| pick(s, &["name", "status"])).collect();
                    data["mcp_servers"] = Value::Array(servers);
                }
                out.push(output(OutputKind::System, text, data));
            }
            Some("permission_denied") => {
                let tool = str_at(v, "tool_name").unwrap_or("a tool");
                let reason = str_at(v, "decision_reason_type")
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default();
                out.push(output(
                    OutputKind::Error,
                    format!("Permission denied: {tool}{reason}. Nobody can approve tools in an unattended run."),
                    v.clone(),
                ));
            }
            Some("thinking_tokens") => {}
            other => {
                let detail = ["message", "status", "content"]
                    .iter()
                    .find_map(|k| str_at(v, k))
                    .map(|d| format!(": {}", one_line(d, SUMMARY_CHARS)))
                    .unwrap_or_default();
                out.push(output(
                    OutputKind::System,
                    format!("{}{detail}", other.unwrap_or("system")),
                    v.clone(),
                ));
            }
        }
    }

    /// Returns false when the line is not shaped like an assistant message.
    fn assistant(&mut self, v: &Value, out: &mut Vec<RunOutput>) -> bool {
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
            return false;
        };
        let mut base = pick(v, &["parent_tool_use_id"]);
        if let Some(id) = v.pointer("/message/id").and_then(Value::as_str) {
            base["message_id"] = json!(id);
        }
        // API errors come back as a synthetic assistant message carrying `error`.
        let error = v.get("error").filter(|e| !e.is_null());
        if let Some(error) = error {
            base["error"] = error.clone();
            for key in ["api_error_status", "api_error_type"] {
                if let Some(x) = v.get(key).filter(|x| !x.is_null()) {
                    base[key] = x.clone();
                }
            }
        }
        for block in blocks {
            match str_at(block, "type") {
                Some("text") => {
                    let kind =
                        if error.is_some() { OutputKind::Error } else { OutputKind::Assistant };
                    out.push(output(kind, str_at(block, "text").unwrap_or(""), base.clone()));
                }
                Some("thinking") => {
                    // The model may return only a signature; there is nothing to show then.
                    let text = str_at(block, "thinking").unwrap_or("");
                    if !text.is_empty() {
                        out.push(output(OutputKind::Thinking, text, base.clone()));
                    }
                }
                Some("redacted_thinking") => {}
                Some("tool_use") => {
                    let name = str_at(block, "name").unwrap_or("tool");
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    if let Some(id) = str_at(block, "id") {
                        if self.tool_names.len() >= TOOL_NAMES_MAX {
                            self.tool_names.clear();
                        }
                        self.tool_names.insert(id.to_owned(), name.to_owned());
                    }
                    let mut data = base.clone();
                    data["id"] = block.get("id").cloned().unwrap_or(Value::Null);
                    data["name"] = json!(name);
                    data["input"] = input.clone();
                    out.push(output(OutputKind::ToolUse, tool_summary(name, &input), data));
                }
                other => {
                    let mut data = base.clone();
                    data["block"] = block.clone();
                    out.push(output(
                        OutputKind::System,
                        format!("[{} block]", other.unwrap_or("unknown")),
                        data,
                    ));
                }
            }
        }
        true
    }

    fn user(&mut self, v: &Value, out: &mut Vec<RunOutput>) -> bool {
        let content = v.pointer("/message/content");
        let blocks = match content {
            Some(Value::Array(blocks)) => blocks.as_slice(),
            // A plain-string user message (a replayed prompt) is not output.
            Some(Value::String(_)) => return true,
            _ => return false,
        };
        let base = pick(v, &["parent_tool_use_id"]);
        for block in blocks {
            match str_at(block, "type") {
                Some("tool_result") => {
                    let id = str_at(block, "tool_use_id");
                    let name = id.and_then(|id| self.tool_names.get(id)).cloned();
                    let is_error = block.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    let content = block.get("content").map(result_text).unwrap_or_default();
                    let preview = cut(content.trim_end(), PREVIEW_CHARS);
                    let text = match (&name, is_error) {
                        (Some(n), true) => format!("{n} failed: {preview}"),
                        (None, true) => format!("failed: {preview}"),
                        _ => preview,
                    };
                    let mut data = base.clone();
                    data["tool_use_id"] = json!(id);
                    data["name"] = json!(name);
                    data["is_error"] = json!(is_error);
                    data["content"] = block.get("content").cloned().unwrap_or(Value::Null);
                    out.push(output(OutputKind::ToolResult, text, data));
                }
                Some("text") => {
                    let mut data = base.clone();
                    data["role"] = json!("user");
                    out.push(output(OutputKind::System, str_at(block, "text").unwrap_or(""), data));
                }
                _ => {}
            }
        }
        true
    }
}

/// The text of a tool result's `content`: a string, or blocks of which the text ones count.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match str_at(b, "type") {
                Some("text") => str_at(b, "text").unwrap_or("").to_owned(),
                Some(other) => format!("[{other}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn result(v: &Value, out: &mut Vec<RunOutput>) {
    let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
    let reason = str_at(v, "terminal_reason").or_else(|| str_at(v, "subtype")).unwrap_or("error");
    let text = match (str_at(v, "result"), is_error) {
        (Some(r), false) => r.to_owned(),
        (Some(r), true) => format!("Failed ({reason}): {r}"),
        (None, _) => format!("Ended: {reason}"),
    };
    let data = pick(
        v,
        &[
            "subtype",
            "is_error",
            "terminal_reason",
            "stop_reason",
            "num_turns",
            "duration_ms",
            "duration_api_ms",
            "total_cost_usd",
            "usage",
            "modelUsage",
            "permission_denials",
            "api_error_status",
            "session_id",
        ],
    );
    out.push(output(OutputKind::Result, text, data));
    let denials = v.get("permission_denials").and_then(Value::as_array).filter(|d| !d.is_empty());
    if let Some(denials) = denials {
        let mut tools: Vec<&str> = denials.iter().filter_map(|d| str_at(d, "tool_name")).collect();
        tools.sort_unstable();
        tools.dedup();
        out.push(output(
            OutputKind::Error,
            format!(
                "{} tool call(s) were denied ({}). Unattended runs cannot ask; allow what the agent needs with allowed_tools or permission_mode in its config.",
                denials.len(),
                tools.join(", ")
            ),
            json!({ "permission_denials": denials }),
        ));
    }
}

fn rate_limit(v: &Value, out: &mut Vec<RunOutput>) {
    let info = v.get("rate_limit_info").unwrap_or(&Value::Null);
    match str_at(info, "status") {
        Some("allowed") => {}
        status => out.push(output(
            OutputKind::System,
            format!("Rate limit: {}", status.unwrap_or("unknown")),
            info.clone(),
        )),
    }
}

#[cfg(test)]
mod tests;
