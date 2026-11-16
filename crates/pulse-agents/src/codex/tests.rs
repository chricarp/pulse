use pulse_contracts::OutputKind;
use pulse_contracts::config::{AgentKind, CodexSettings};

use super::*;
use crate::adapter_for;
use crate::test_support::{
    Expect, HOSTILE, agent, assert_outputs, env, parse, parse_stdout, request,
};

fn codex(settings: CodexSettings) -> AgentConfig {
    agent(AdapterSettings::Codex(settings))
}

fn invocation(settings: CodexSettings, subject: &str) -> Result<Invocation, AdapterError> {
    adapter_for(AgentKind::Codex).unwrap().invocation(&codex(settings), &request(subject))
}

const DEFAULT_ARGS: [&str; 4] =
    ["exec", "--json", "--sandbox=read-only", "--config=approval_policy=\"never\""];

#[test]
fn defaults_do_not_widen_permissions() {
    let inv = invocation(CodexSettings::default(), "/u/a.pdf").unwrap();
    assert_eq!(inv.program, "codex");
    assert_eq!(inv.args, DEFAULT_ARGS);
    assert_eq!(inv.stdin.as_deref(), Some("Summarize /u/a.pdf"));
    assert_eq!(env(&inv.env, "PULSE_CHAIN_DEPTH"), Some("1"));
}

#[test]
fn event_content_never_reaches_argv() {
    for subject in HOSTILE {
        let inv = invocation(CodexSettings::default(), subject).unwrap();
        assert_eq!(inv.args, DEFAULT_ARGS, "{subject:?}");
        assert!(inv.stdin.unwrap().contains(subject));
    }
}

#[test]
fn configured_values() {
    let inv = invocation(
        CodexSettings {
            program: Some("/opt/codex".into()),
            model: Some("--dangerously-bypass-approvals-and-sandbox".into()),
            sandbox: Some("workspace-write".into()),
            approval_policy: Some("on-request".into()),
            extra_args: vec!["--skip-git-repo-check".into()],
        },
        "/u/a.pdf",
    )
    .unwrap();
    assert_eq!(inv.program, "/opt/codex");
    assert_eq!(
        inv.args,
        [
            "exec",
            "--json",
            "--sandbox=workspace-write",
            "--config=approval_policy=\"on-request\"",
            "--model=--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
        ]
    );
    for bad in ["never\"\nsandbox_mode=\"danger-full-access", "", "on request", "\"never\""] {
        let err = invocation(
            CodexSettings { approval_policy: Some(bad.into()), ..Default::default() },
            "/u/a",
        )
        .unwrap_err();
        assert!(matches!(err, AdapterError::Invalid(_)), "{bad:?}");
    }
}

#[test]
fn recorded_and_synthetic_streams() {
    use OutputKind::*;
    let cases: [(&str, &str, Expect); 4] = [
        (
            "answer",
            include_str!("../../tests/fixtures/codex/answer.jsonl"),
            &[(System, "Codex session"), (Assistant, "2 + 2 = 4."), (Result, "2 + 2 = 4.")],
        ),
        (
            "sandbox-failed",
            include_str!("../../tests/fixtures/codex/sandbox-failed.jsonl"),
            &[
                (System, "Codex session"),
                (Assistant, "I’ll read `note.txt`"),
                (Assistant, "I couldn’t access `note.txt`"),
                (Result, "I couldn’t access `note.txt`"),
            ],
        ),
        (
            "bad-model",
            include_str!("../../tests/fixtures/codex/bad-model.jsonl"),
            &[
                (System, "Codex session"),
                (Error, "Model metadata for `pulse-no-such-model` not found."),
                (Error, "The 'pulse-no-such-model' model is not supported"),
                (Error, "Turn failed: The 'pulse-no-such-model' model is not supported"),
            ],
        ),
        (
            "tools (synthetic)",
            include_str!("../../tests/fixtures/codex/tools.synthetic.jsonl"),
            &[
                (System, "Codex session"),
                (Thinking, "**Reading the note**"),
                (System, "Plan:\n[ ] Read note.txt\n[ ] Write out.txt"),
                (ToolUse, "$ bash -lc 'cat note.txt'"),
                (ToolResult, "exit 0\nPineapple tastes sweet."),
                (ToolUse, "$ bash -lc 'printf Pineapple > out.txt'"),
                (ToolResult, "exit 1 (failed)\nbash: out.txt: Read-only file system"),
                (ToolUse, "patch (failed): add /tmp/pulse-run/out.txt"),
                (ToolUse, "pulse.query_events {\"limit\":1}"),
                (ToolResult, "query_events {\"content\""),
                (ToolUse, "web search pineapple sweetness"),
                (System, "Plan:\n[x] Read note.txt\n[ ] Write out.txt"),
                (Assistant, "I read note.txt"),
                (Result, "I read note.txt"),
            ],
        ),
    ];
    for (name, text, want) in cases {
        assert_outputs(&parse_stdout(CodexAdapter.parser(), text), want, name);
    }
}

#[test]
fn provider_detail_is_kept_in_data() {
    let out = parse_stdout(
        CodexAdapter.parser(),
        include_str!("../../tests/fixtures/codex/answer.jsonl"),
    );
    assert_eq!(out[0].data.as_ref().unwrap()["thread_id"], "00000000-0000-4000-8000-000000000001");
    assert_eq!(out[2].data.as_ref().unwrap()["usage"]["output_tokens"], 12);
    let tools = parse_stdout(
        CodexAdapter.parser(),
        include_str!("../../tests/fixtures/codex/tools.synthetic.jsonl"),
    );
    let failed = tools[6].data.as_ref().unwrap();
    assert_eq!(failed["exit_code"], 1);
    assert_eq!(failed["status"], "failed");
}

#[test]
fn unknown_and_malformed_lines_degrade_to_raw_output() {
    use OutputKind::*;
    let cases: [(Stream, &str, Expect); 8] = [
        (
            Stream::Stdout,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_mess"#,
            &[(Stdout, "{\"type\":\"item.completed\"")],
        ),
        (
            Stream::Stdout,
            r#"{"type":"session.configured","model":"x"}"#,
            &[(Stdout, "{\"type\":\"session.configured\"")],
        ),
        (
            Stream::Stdout,
            r#"{"type":"item.completed"}"#,
            &[(Stdout, "{\"type\":\"item.completed\"}")],
        ),
        (
            Stream::Stdout,
            r#"{"type":"item.completed","item":{"id":"i","type":"hologram"}}"#,
            &[(System, "[hologram]")],
        ),
        (
            Stream::Stdout,
            r#"{"type":"item.updated","item":{"id":"i","type":"command_execution","command":"ls"}}"#,
            &[],
        ),
        (Stream::Stdout, r#"{"type":"turn.completed"}"#, &[(Result, "Turn completed")]),
        (Stream::Stdout, "42", &[(Stdout, "42")]),
        (
            Stream::Stderr,
            "Reading prompt from stdin...",
            &[(Stderr, "Reading prompt from stdin...")],
        ),
    ];
    for (stream, line, want) in cases {
        assert_outputs(&parse(CodexAdapter.parser(), &[(stream, line)]), want, line);
    }
}
