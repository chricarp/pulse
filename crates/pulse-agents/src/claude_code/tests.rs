use pulse_contracts::OutputKind;
use pulse_contracts::config::{AgentKind, ClaudeCodeSettings, CodexSettings};

use super::*;
use crate::adapter_for;
use crate::test_support::{
    Expect, HOSTILE, agent, assert_outputs, env, parse, parse_stdout, request,
};

fn claude(settings: ClaudeCodeSettings) -> AgentConfig {
    agent(AdapterSettings::ClaudeCode(settings))
}

fn invocation(settings: ClaudeCodeSettings, subject: &str) -> Invocation {
    adapter_for(AgentKind::ClaudeCode)
        .unwrap()
        .invocation(&claude(settings), &request(subject))
        .unwrap()
}

const DEFAULT_ARGS: [&str; 5] = [
    "--print",
    "--output-format=stream-json",
    "--verbose",
    "--permission-mode=dontAsk",
    "--permission-prompts=none",
];

#[test]
fn defaults_do_not_widen_permissions() {
    let inv = invocation(ClaudeCodeSettings::default(), "/u/a.pdf");
    assert_eq!(inv.program, "claude");
    assert_eq!(inv.args, DEFAULT_ARGS);
    assert_eq!(inv.stdin.as_deref(), Some("Summarize /u/a.pdf"));
    assert_eq!(env(&inv.env, "PULSE_AGENT"), Some("notify"));
    assert_eq!(env(&inv.env, "PULSE_RUN_ID"), Some(request("").run_id.to_string().as_str()));
    assert_eq!(env(&inv.env, "TOKEN"), Some("t"));
}

#[test]
fn event_content_never_reaches_argv() {
    for subject in HOSTILE {
        let inv = invocation(ClaudeCodeSettings::default(), subject);
        assert_eq!(inv.args, DEFAULT_ARGS, "{subject:?}");
        assert!(inv.stdin.unwrap().contains(subject));
    }
}

#[test]
fn configured_values_stay_attached_to_their_flags() {
    let inv = invocation(
        ClaudeCodeSettings {
            program: Some("/opt/claude".into()),
            model: Some("--dangerously-skip-permissions".into()),
            permission_mode: Some("acceptEdits".into()),
            allowed_tools: vec!["Bash(git diff:*)".into(), "--dangerously-skip-permissions".into()],
            disallowed_tools: vec!["WebFetch".into()],
            extra_args: vec!["--max-budget-usd=0.5".into()],
        },
        "/u/a.pdf",
    );
    assert_eq!(inv.program, "/opt/claude");
    assert_eq!(
        inv.args,
        [
            "--print",
            "--output-format=stream-json",
            "--verbose",
            "--permission-mode=acceptEdits",
            "--permission-prompts=none",
            "--model=--dangerously-skip-permissions",
            "--allowedTools=Bash(git diff:*)",
            "--allowedTools=--dangerously-skip-permissions",
            "--disallowedTools=WebFetch",
            "--max-budget-usd=0.5",
        ]
    );
}

#[test]
fn a_configured_prompt_handler_replaces_the_deny_default() {
    let inv = invocation(
        ClaudeCodeSettings {
            extra_args: vec!["--permission-prompt-tool=mcp__approver__ask".into()],
            ..Default::default()
        },
        "/u/a.pdf",
    );
    assert!(!inv.args.iter().any(|a| a == "--permission-prompts=none"), "{:?}", inv.args);
}

#[test]
fn invalid_requests() {
    let adapter = adapter_for(AgentKind::ClaudeCode).unwrap();
    let mut req = request("/u/a.pdf");
    req.prompt = None;
    let err = adapter.invocation(&claude(ClaudeCodeSettings::default()), &req).unwrap_err();
    assert!(matches!(err, AdapterError::Invalid(_)), "{err}");
    let codex = agent(AdapterSettings::Codex(CodexSettings::default()));
    let err = adapter.invocation(&codex, &request("/u/a.pdf")).unwrap_err();
    assert!(
        matches!(err, AdapterError::WrongAdapter { configured: AgentKind::Codex, .. }),
        "{err}"
    );
}

#[test]
fn recorded_streams() {
    use OutputKind::*;
    let cases: [(&str, &str, Expect); 3] = [
        (
            "answer",
            include_str!("../../tests/fixtures/claude-code/answer.jsonl"),
            &[
                (
                    System,
                    "Claude Code 2.1.325 started: model claude-haiku-5-5, permission mode dontAsk",
                ),
                (Assistant, "4."),
                (Result, "4."),
            ],
        ),
        (
            "tool-denied",
            include_str!("../../tests/fixtures/claude-code/tool-denied.jsonl"),
            &[
                (System, "Claude Code"),
                (ToolUse, "Read /tmp/pulse-run/note.txt"),
                (ToolResult, "1\tPineapple tastes sweet."),
                (ToolUse, "Write /tmp/pulse-run/out.txt"),
                (Error, "Permission denied: Write (mode)."),
                (ToolResult, "Write failed: Permission to use Write has been denied"),
                (Assistant, "I couldn't create `out.txt`."),
                (Result, "I couldn't create `out.txt`."),
                (Error, "1 tool call(s) were denied (Write)."),
            ],
        ),
        (
            "bad-model",
            include_str!("../../tests/fixtures/claude-code/bad-model.jsonl"),
            &[
                (System, "Claude Code"),
                (Error, "There's an issue with the selected model (pulse-no-such-model)."),
                (Result, "Failed (api_error): There's an issue"),
            ],
        ),
    ];
    for (name, text, want) in cases {
        assert_outputs(&parse_stdout(ClaudeCodeAdapter.parser(), text), want, name);
    }
}

#[test]
fn provider_detail_is_kept_in_data() {
    let out = parse_stdout(
        ClaudeCodeAdapter.parser(),
        include_str!("../../tests/fixtures/claude-code/tool-denied.jsonl"),
    );
    let init = out[0].data.as_ref().unwrap();
    assert_eq!(init["session_id"], "00000000-0000-4000-8000-000000000001");
    assert_eq!(init["cwd"], "/tmp/pulse-run");
    assert!(init.get("skills").is_none(), "only what a run view needs");
    let read = out[1].data.as_ref().unwrap();
    assert_eq!(read["input"]["file_path"], "/tmp/pulse-run/note.txt");
    let denied_result = out[5].data.as_ref().unwrap();
    assert_eq!(denied_result["is_error"], true);
    assert_eq!(denied_result["name"], "Write");
    let result = out[7].data.as_ref().unwrap();
    assert_eq!(result["total_cost_usd"], 0.00411251);
    assert_eq!(result["num_turns"], 3);
    assert_eq!(result["permission_denials"][0]["tool_name"], "Write");
    assert!(result["usage"]["output_tokens"].is_number());
}

#[test]
fn unknown_and_malformed_lines_degrade_to_raw_output() {
    use OutputKind::*;
    let truncated = &include_str!("../../tests/fixtures/claude-code/answer.jsonl")[..300];
    let cases: [(Stream, &str, Expect); 13] = [
        (Stream::Stdout, truncated, &[(Stdout, "{\"type\":\"system\"")]),
        (Stream::Stdout, "not json at all", &[(Stdout, "not json at all")]),
        (
            Stream::Stdout,
            r#"{"type":"brand_new_event","x":1}"#,
            &[(Stdout, "{\"type\":\"brand_new_event\"")],
        ),
        (Stream::Stdout, "[1,2,3]", &[(Stdout, "[1,2,3]")]),
        (Stream::Stdout, r#"{"type":"assistant"}"#, &[(Stdout, "{\"type\":\"assistant\"}")]),
        (
            Stream::Stdout,
            r#"{"type":"assistant","message":{"content":[{"type":"image","source":{}}]}}"#,
            &[(System, "[image block]")],
        ),
        (
            Stream::Stdout,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":7}]}}"#,
            &[(Assistant, "")],
        ),
        (
            Stream::Stdout,
            r#"{"type":"system","subtype":"compact_boundary","message":"compacted\nhistory"}"#,
            &[(System, "compact_boundary: compacted history")],
        ),
        (Stream::Stdout, r#"{"type":"user","message":{"content":"replayed prompt"}}"#, &[]),
        (Stream::Stdout, r#"{"type":"result"}"#, &[(Result, "Ended: error")]),
        (
            Stream::Stdout,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
            &[(System, "Rate limit: rejected")],
        ),
        (Stream::Stdout, "", &[]),
        (
            Stream::Stderr,
            r#"{"type":"result","result":"from stderr"}"#,
            &[(Stderr, "{\"type\":\"result\"")],
        ),
    ];
    for (stream, line, want) in cases {
        assert_outputs(&parse(ClaudeCodeAdapter.parser(), &[(stream, line)]), want, line);
    }
}
