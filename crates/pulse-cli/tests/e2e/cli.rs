//! The CLI as scripts and people use it: exit codes, `--json` shapes, `pulse run` passing its
//! command's exit through, and adding definitions without opening the config file.

use pulse_contracts::status::SourceState;
use serde_json::Value;

use crate::support::*;

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// Without a daemon: commands that need one exit 3 and say how to start it; `stop` is a no-op;
/// usage errors exit 2; `pulse run` still runs the command, passes its exit status through
/// (128 + signal when killed, 127 when it cannot start), and says nothing of its own.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_daemon() {
    let env = Env::new();
    for args in [&["status"][..], &["runs", "list"], &["tail"]] {
        let (code, out, err) = env.run(args).await;
        assert_eq!(code, 3, "{args:?}: {out}{err}");
        assert!(err.contains("pulse start"), "{args:?}: {err}");
    }
    let (code, out, _) = env.run(&["stop"]).await;
    assert_eq!((code, out.trim()), (0, "Pulse is not running."));
    let (code, _, err) = env.run(&["rules", "frobnicate"]).await;
    assert_eq!(code, 2, "{err}");

    let cases: &[(&[&str], i32, &str)] = &[
        (&["sh", "-c", "echo out; echo err >&2; exit 5"], 5, "out\n"),
        (&["sh", "-c", "kill -TERM $$"], 128 + 15, ""),
        (&["sh", "-c", "kill -KILL $$"], 128 + 9, ""),
        (&["true"], 0, ""),
        (&["no-such-command-e2e"], 127, ""),
    ];
    for (command, want, stdout) in cases {
        let mut args = vec!["run", "--"];
        args.extend_from_slice(command);
        let (code, out, err) = env.run(&args).await;
        assert_eq!(code, *want, "{command:?}: {err}");
        assert_eq!(out, *stdout, "{command:?}");
        if *want != 127 {
            assert!(!err.contains("pulse"), "silent without a daemon: {err:?}");
        }
    }

    // init: a starter file once, then templates added to it; an unknown template is an error.
    let (code, out, _) = env.run(&["init"]).await;
    assert_eq!(code, 0);
    assert!(out.starts_with("Wrote "), "{out}");
    let (code, out, _) = env.run(&["init"]).await;
    assert_eq!(code, 0);
    assert!(out.contains("already exists; nothing changed"), "{out}");
    let (code, _, err) = env.run(&["init", "--template", "nope"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("pulse init --list"), "{err}");
    let (code, _, _) = env.run(&["init", "--template", "build-failure"]).await;
    assert_eq!(code, 0);
    assert!(env.read_config().contains("name = \"build-failed\""));
}

/// Sources, agents and rules added and removed from the CLI, written to `pulse.toml` with its
/// comments kept, with warnings when an addition cannot work and refusals that say why.
#[tokio::test(flavor = "multi_thread")]
async fn adding_and_removing_definitions() {
    let env = Env::new();
    env.mkdir("inbox");
    env.write_config("# my settings\n[limits]\nmax_chain_depth = 2 # keep\n");
    let daemon = env.start().await;

    let (code, out, err) = env.run(&["--json", "sources", "add", "fs", "inbox", "~/inbox"]).await;
    assert_eq!(code, 0, "{err}");
    let s = json(&out);
    assert_eq!(
        (s["config"]["name"].as_str(), s["config"]["kind"].as_str()),
        (Some("inbox"), Some("fs"))
    );
    assert!(s["health"]["state"].is_string(), "{s}");
    // A source that cannot watch: `status` points at `pulse sources` right under the count.
    let (code, _, err) = env.run(&["sources", "add", "fs", "gone", "~/gone"]).await;
    assert_eq!(code, 0, "{err}");
    daemon.watch().await.source_state(Some("gone"), SourceState::Degraded).await;
    let (_, out, _) = env.run(&["status"]).await;
    let lines: Vec<&str> = out.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("sources ")).unwrap();
    assert!(lines[at + 1].trim().starts_with("`pulse sources` says"), "{out}");
    let (code, _, err) = env.run(&["sources", "remove", "gone"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = env.run(&["sources", "add", "schedule", "tick", "--every", "1h"]).await;
    assert_eq!(code, 0, "{err}");
    // Enable and disable change only `enabled` in the file, from the CLI as from the UI.
    let (code, out, err) = env.run(&["sources", "disable", "tick"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Disabled source tick"), "{out}");
    assert!(env.read_config().contains("every = \"1h\"\nenabled = false"), "{}", env.read_config());
    let (code, _, err) = env.run(&["sources", "enable", "tick"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(!env.read_config().contains("enabled = false"), "{}", env.read_config());
    let (code, _, err) = env.run(&["sources", "disable", "nope"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("nope"), "{err}");
    let (code, _, err) = env.run(&["sources", "add", "schedule", "bad"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("cron") || err.contains("every"), "{err}");

    let (code, out, err) = env
        .run_stdin(
            &["agents", "add", "--file", "-"],
            "name = \"notes\"\nadapter = \"command\"\ncommand = [\"true\"]\nallow = [\"fs.*:~/elsewhere/**\"]\n",
        )
        .await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Saved agent notes"), "{out}");

    // A rule its agent's scopes can never cover is saved, with a warning saying why.
    let rule = env.dir.path().join("rule.toml");
    std::fs::write(
        &rule,
        "name = \"new-note\"\non = \"fs.file.created\"\nwhere = { path = \"~/inbox/**\" }\n[run]\nagent = \"notes\"\n",
    )
    .unwrap();
    let (code, out, err) = env.run(&["rules", "add", "--file", rule.to_str().unwrap()]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("# new-note"), "{out}");
    assert!(err.contains("warning: this rule can never fire"), "{err}");
    let (_, out, _) = env.run(&["--json", "status"]).await;
    let status = json(&out);
    assert_eq!(status["status"]["config_warnings"], 1, "{status}");
    assert_eq!(status["config_warnings"][0]["item"], "rule \"new-note\"");

    // Fixing the agent clears it. `add` does not replace by accident: that takes --replace.
    let fixed = "name = \"notes\"\nadapter = \"command\"\ncommand = [\"true\"]\nallow = [\"fs.*:~/inbox/**\"]\n";
    let (code, _, err) = env.run_stdin(&["agents", "add", "--file", "-"], fixed).await;
    assert_eq!(code, 1);
    assert!(err.contains("already exists; pass --replace"), "{err}");
    let (code, _, err) = env.run_stdin(&["agents", "add", "--replace", "--file", "-"], fixed).await;
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = env.run(&["--json", "status"]).await;
    assert_eq!(json(&out)["status"]["config_warnings"], 0);

    // The shapes scripts read.
    let (_, out, _) = env.run(&["--json", "rules", "list"]).await;
    let rules = json(&out);
    assert_eq!(rules["rules"][0]["config"]["name"], "new-note");
    assert!(rules["rules"][0]["state"].is_object() && rules["rules"][0]["origin"].is_object());
    let (_, out, _) = env.run(&["--json", "agents"]).await;
    assert_eq!(json(&out)["agents"][0]["config"]["adapter"], "command");
    let (_, out, _) = env.run(&["--json", "events"]).await;
    assert!(json(&out)["events"].is_array());
    let (_, out, _) = env.run(&["--json", "runs", "list"]).await;
    assert!(json(&out)["runs"].is_array());
    let (_, out, _) = env
        .run_stdin(
            &["--json", "ingest", "-s", "ci"],
            "{\"type\":\"failed\",\"subject\":\"main\"}\n",
        )
        .await;
    assert_eq!(json(&out), serde_json::json!({ "accepted": 1, "duplicates": 0 }));

    // Removing: an agent still used by a rule is refused with the reason.
    let (code, _, err) = env.run(&["agents", "remove", "notes"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("new-note"), "{err}");
    for args in [
        &["rules", "remove", "new-note"][..],
        &["agents", "remove", "notes"],
        &["sources", "remove", "inbox"],
    ] {
        let (code, _, err) = env.run(args).await;
        assert_eq!(code, 0, "{args:?}: {err}");
    }
    let (code, _, err) = env.run(&["rules", "remove", "new-note"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("new-note"), "{err}");
    let config = env.read_config();
    assert!(
        config.starts_with("# my settings\n[limits]\nmax_chain_depth = 2 # keep\n"),
        "{config}"
    );
    assert!(!config.contains("new-note") && !config.contains("inbox"), "{config}");
    assert!(config.contains("name = \"tick\""), "{config}");
    daemon.stop().await;
}

/// `pulse project trust` shows what it would trust and binds to the hash it showed: a file
/// replaced between looking and trusting is not trusted.
#[tokio::test(flavor = "multi_thread")]
async fn trusting_a_project_binds_to_the_content_shown() {
    let env = Env::new();
    let repo = env.mkdir("code/repo");
    let file = repo.join("pulse.toml");
    let text = "[[agent]]\nname = \"fmt\"\nadapter = \"command\"\ncommand = [\"prettier\", \"--write\", \"--\", \"{{path}}\"]\nallow = [\"fs.*:~/code/repo/**\"]\n";
    std::fs::write(&file, text).unwrap();
    let daemon = env.start().await;
    let dir = repo.to_str().unwrap();

    let (code, out, err) = env.run(&["project", "trust", dir]).await;
    assert_eq!(code, 1, "a review trusts nothing");
    assert!(out.contains("prettier") && out.contains("content hash"), "{out}");
    assert!(err.contains("Nothing was trusted") && err.contains("--hash"), "{err}");
    let (_, out, _) = env.run(&["--json", "project", "trust", dir]).await;
    let hash = json(&out)["error"]["data"]["hash"].as_str().unwrap().to_owned();

    std::fs::write(&file, text.replace("prettier", "curl")).unwrap();
    let (code, _, err) = env.run(&["project", "trust", dir, "--hash", &hash]).await;
    assert_eq!(code, 1);
    assert!(err.contains("changed since that hash was printed"), "{err}");
    let (_, out, _) = env.run(&["--json", "agents"]).await;
    assert_eq!(json(&out)["agents"], serde_json::json!([]));

    std::fs::write(&file, text).unwrap();
    let (code, out, err) = env.run(&["project", "trust", dir, "--hash", &hash]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("trusted"), "{out}");
    let (_, out, _) = env.run(&["--json", "agents"]).await;
    assert_eq!(json(&out)["agents"][0]["config"]["name"], "repo/fmt");
    daemon.stop().await;
}

/// The daemon has stopped, `daemon.json` is stale, and another program now listens on its port.
/// No CLI client sends it the token, an agent token or a login code, and each says what is
/// wrong. The program records everything it receives and answers 404.
#[tokio::test(flavor = "multi_thread")]
async fn clients_refuse_a_program_that_took_the_port() {
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let env = Env::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received: Arc<Mutex<Vec<u8>>> = Arc::default();
    let log = received.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 {
                        return;
                    }
                    log.lock().unwrap().extend_from_slice(&buf[..n]);
                    if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        let reply = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
                        if s.write_all(reply).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    let token = "5ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7";
    std::fs::write(env.home.join("token"), token).unwrap();
    let info = format!(
        r#"{{"pid": 1, "port": {port}, "version": "0.0.0", "started_at": "2026-11-16T00:00:00Z"}}"#
    );
    std::fs::write(env.home.join("daemon.json"), info).unwrap();

    for args in [&["status"][..], &["open", "--print"], &["open"], &["rules", "list"]] {
        let (code, out, err) = env.run(args).await;
        assert_ne!(code, 0, "{args:?}: {out}");
        assert!(err.contains("something else is listening on Pulse's port"), "{args:?}: {err}");
        assert!(!out.contains("login="), "{args:?}: no login link for an unproven port: {out}");
    }
    let (code, _, err) = env.run_stdin(&["ingest", "-s", "ci"], "{\"type\":\"x\"}\n").await;
    assert_ne!(code, 0);
    assert!(err.contains("something else is listening"), "{err}");
    // `pulse run` stays silent about the daemon, as it does when there is none.
    let (code, _, err) = env.run(&["run", "--", "true"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = env.run(&["mcp", "--agent", "x"]).await;
    assert_ne!(code, 0, "{out}");
    assert!(err.contains("something else is listening"), "{err}");

    let seen = String::from_utf8_lossy(&received.lock().unwrap()).into_owned();
    assert!(seen.contains("GET /auth/proof?nonce="), "the clients did ask: {seen}");
    assert!(!seen.contains(token), "the token reached the program on the port:\n{seen}");
    assert!(!seen.contains("pa.x."), "an agent token reached the program on the port:\n{seen}");
    assert!(!seen.contains("login"), "{seen}");
}

/// `--json` streams (`tail`, `runs logs`) are JSON as serde wrote it, whatever the event or
/// the run printed: a bidi override and a DEL in a subject and in run output decode back to
/// themselves instead of breaking the line with a terminal escape.
#[tokio::test(flavor = "multi_thread")]
async fn json_streams_are_json_whatever_the_content() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let hostile = "invoice\u{202e}txt.exe\u{7f}";
    let env = Env::new();
    env.write_config(
        r#"
[[agent]]
name = "printer"
adapter = "command"
command = ["printf", "out \u202Eexe.txt\u007F\n"]
allow = ["ext.*"]

[[rule]]
name = "print"
on = "ext.ci.*"
run = { agent = "printer" }
"#,
    );
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;

    let mut tail = env.pulse();
    tail.args(["--json", "tail"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut tail = tail.spawn().unwrap();
    let mut out = BufReader::new(tail.stdout.take().unwrap()).lines();
    let mut err = BufReader::new(tail.stderr.take().unwrap()).lines();
    // Subscribed once it says so (for people, on stderr).
    within("tail to subscribe", err.next_line()).await.unwrap().unwrap();

    let line = serde_json::json!({ "type": "built", "subject": hostile }).to_string();
    let (code, _, e) = env.run_stdin(&["ingest", "-s", "ci"], &format!("{line}\n")).await;
    assert_eq!(code, 0, "{e}");
    let run = watch.run_in("the run", |_| true).await;
    watch.run_terminal(run.id).await;

    let mut subjects = Vec::new();
    while subjects.is_empty() {
        let line = within("a tail line", out.next_line()).await.unwrap().unwrap();
        let note = json(&line);
        if note["method"] == "events.batch" {
            subjects.extend(note["params"]["events"].as_array().unwrap().iter().cloned());
        }
    }
    assert_eq!(subjects[0]["subject"], hostile, "{subjects:?}");
    drop(tail);

    let (code, out, e) = env.run(&["--json", "runs", "logs", &run.id.to_string()]).await;
    assert_eq!(code, 0, "{e}");
    let texts: Vec<String> =
        out.lines().map(|l| json(l)["text"].as_str().unwrap().to_owned()).collect();
    assert!(texts.iter().any(|t| t == "out \u{202e}exe.txt\u{7f}"), "{texts:?}");

    // Ids as lists show them: the last characters, in any case, down to three.
    let id = run.id.to_string();
    for short in [&id[id.len() - 8..], &id[id.len() - 3..].to_ascii_lowercase()] {
        let (code, out, e) = env.run(&["--json", "runs", "show", short]).await;
        assert_eq!(code, 0, "{short}: {e}{out}");
        assert_eq!(json(&out)["id"], id.as_str());
    }
    let event = run.event.to_string();
    let (code, out, e) = env.run(&["--json", "events", "show", &event[event.len() - 8..]]).await;
    assert_eq!(code, 0, "{e}{out}");
    assert_eq!(json(&out)["event"]["id"], event.as_str());
    daemon.stop().await;
}

/// Every command path `pulse help` lists, recursively, without `help` itself.
async fn command_paths(env: &Env) -> Vec<String> {
    let mut found = Vec::new();
    let mut queue = vec![String::new()];
    while let Some(path) = queue.pop() {
        let mut args: Vec<&str> = path.split_whitespace().collect();
        args.push("--help");
        let (code, help, err) = env.run(&args).await;
        assert_eq!(code, 0, "{args:?}: {err}");
        let Some(list) = help.split("Commands:\n").nth(1) else { continue };
        for line in list.lines().take_while(|l| !l.is_empty()) {
            // `  name  Description`; wrapped descriptions are indented further.
            let Some(name) = line.strip_prefix("  ").filter(|l| !l.starts_with(' ')) else {
                continue;
            };
            let name = name.split_whitespace().next().unwrap();
            if name != "help" {
                let child = format!("{path} {name}").trim().to_owned();
                found.push(child.clone());
                queue.push(child);
            }
        }
    }
    found.sort();
    found
}

/// `pulse help`, all the way down: every command, argument and option says what it is.
#[tokio::test(flavor = "multi_thread")]
async fn help_describes_every_command_and_argument() {
    let env = Env::new();
    let mut paths = command_paths(&env).await;
    paths.push(String::new());
    let mut missing = Vec::new();
    for path in paths {
        let mut args: Vec<&str> = path.split_whitespace().collect();
        args.push("--help");
        let (_, help, _) = env.run(&args).await;
        let mut section = false;
        for line in help.lines() {
            if ["Commands:", "Arguments:", "Options:"].contains(&line) {
                section = true;
                continue;
            }
            if line.is_empty() {
                section = false;
            }
            // `  name   Description`, or a description wrapped onto the next line.
            let item = line.trim_start();
            if section
                && line.starts_with("  ")
                && (item.starts_with('-') || line.len() - item.len() == 2)
            {
                let described = item.split("  ").filter(|p| !p.trim().is_empty()).count() > 1;
                if !described {
                    missing.push(format!("pulse {path}: {}", item.trim_end()));
                }
            }
        }
    }
    assert!(missing.is_empty(), "no help for:\n{}", missing.join("\n"));
}

#[derive(Clone, Copy, Debug)]
enum Want {
    Ok,
    Err(&'static str),
}

/// The `--json` contract (docs/user/cli.md), for every command, with and without a daemon:
/// stdout is exactly one JSON document, either the answer (exit 0) or `{"error": {kind,
/// message, code?, data?}}` (exit 2 for `usage`, 3 for `not_running`, else 1) with nothing on
/// stderr. A command missing from the table fails the test.
#[tokio::test(flavor = "multi_thread")]
async fn every_command_answers_json_or_a_json_error() {
    use Want::*;
    const NO_ID: &str = "01KA0000000000000000000000";
    // Not part of the contract: `start` is the daemon, `run` is the command it wraps, `mcp`
    // keeps stdout for MCP.
    const EXEMPT: &[&str] = &["start", "run", "mcp"];
    let env = Env::new();
    let project = env.mkdir("code/repo");
    std::fs::write(project.join("pulse.toml"), "").unwrap();
    let project = project.to_str().unwrap();
    let rule = "name = \"r\"\non = \"ext.ci.*\"\n[run]\nagent = \"a\"\n";
    env.write_config(
        "[[agent]]\nname = \"a\"\nadapter = \"command\"\ncommand = [\"true\"]\nallow = [\"ext.*\"]\n\n[[rule]]\nname = \"r\"\non = \"ext.ci.*\"\nrun = { agent = \"a\" }\n",
    );
    #[rustfmt::skip]
    let cases: &[(&[&str], &str, Want, Want)] = &[
        // (arguments after --json, stdin, without a daemon, with one)
        (&["status"], "", Err("not_running"), Ok),
        (&["pause"], "", Err("not_running"), Ok),
        (&["resume"], "", Err("not_running"), Ok),
        (&["open", "--print"], "", Err("not_running"), Ok),
        (&["open"], "", Err("not_running"), Ok),
        (&["init"], "", Ok, Ok),
        (&["init", "--list"], "", Ok, Ok),
        (&["init", "--template", "nope"], "", Err("invalid_params"), Err("invalid_params")),
        (&["tail", "--type", "a b"], "", Err("not_running"), Err("invalid_params")),
        (&["events"], "", Err("not_running"), Ok),
        (&["events", "show", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["replay", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["ingest", "-s", "ci"], "{\"type\":\"x\"}\n", Err("not_running"), Ok),
        // Parsed as `POST /ingest` parses a JSON line: an unknown field is a typo, not data to
        // drop; the lines before a bad one are sent.
        (&["ingest", "-s", "ci"], "{\"type\":\"a\"}\n{\"type\":\"x\",\"colour\":\"red\"}\n", Err("not_running"), Err("invalid_params")),
        (&["ingest", "-s", "ci"], "{\"specversion\":\"1.0\",\"id\":\"1\",\"source\":\"ci\",\"type\":\"x\"}\n", Err("not_running"), Ok),
        (&["test-rule", "r", "--event", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["rules", "list"], "", Err("not_running"), Ok),
        (&["rules"], "", Err("not_running"), Ok),
        (&["rules", "show", "nope"], "", Err("not_running"), Err("not_found")),
        (&["rules", "add", "--file", "-"], rule, Err("not_running"), Err("stale")),
        (&["rules", "enable", "nope"], "", Err("not_running"), Err("not_found")),
        (&["rules", "disable", "nope"], "", Err("not_running"), Err("not_found")),
        (&["rules", "snooze", "nope", "1h"], "", Err("not_running"), Err("not_found")),
        (&["rules", "snooze", "r", "soon"], "", Err("not_running"), Err("invalid_params")),
        (&["rules", "unsnooze", "nope"], "", Err("not_running"), Err("not_found")),
        (&["rules", "archive", "nope"], "", Err("not_running"), Err("not_found")),
        (&["rules", "reopen", "nope"], "", Err("not_running"), Err("not_found")),
        (&["rules", "remove", "nope"], "", Err("not_running"), Err("not_found")),
        (&["rules", "history", "r"], "", Err("not_running"), Ok),
        (&["runs", "list"], "", Err("not_running"), Ok),
        (&["runs"], "", Err("not_running"), Ok),
        (&["runs", "show", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["runs", "logs", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["runs", "approve", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["runs", "reject", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["runs", "cancel", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["runs", "rerun", NO_ID], "", Err("not_running"), Err("not_found")),
        (&["runs", "show", "not-an-id"], "", Err("not_running"), Err("invalid_params")),
        // A short id is the end of one (what lists show); none ends in this.
        (&["runs", "show", "ZZZZZZZZ"], "", Err("not_running"), Err("not_found")),
        (&["events", "show", "zzzz"], "", Err("not_running"), Err("not_found")),
        (&["agents"], "", Err("not_running"), Ok),
        (&["agents", "list"], "", Err("not_running"), Ok),
        (&["agents", "show", "nope"], "", Err("not_running"), Err("not_found")),
        (&["agents", "add", "--file", "-"], "not toml", Err("not_running"), Err("invalid_params")),
        (&["agents", "remove", "nope"], "", Err("not_running"), Err("not_found")),
        (&["agents", "mcp-credentials", "nope"], "", Err("not_running"), Err("not_found")),
        (&["sources"], "", Err("not_running"), Ok),
        (&["sources", "list"], "", Err("not_running"), Ok),
        (&["sources", "add", "schedule", "bad"], "", Err("not_running"), Err("config_invalid")),
        (&["sources", "remove", "nope"], "", Err("not_running"), Err("not_found")),
        (&["sources", "enable", "nope"], "", Err("not_running"), Err("not_found")),
        (&["sources", "disable", "nope"], "", Err("not_running"), Err("not_found")),
        (&["project", "list"], "", Err("not_running"), Ok),
        (&["project"], "", Err("not_running"), Ok),
        (&["project", "trust", project], "", Err("not_running"), Err("not_trusted")),
        (&["project", "untrust", project], "", Err("not_running"), Err("not_found")),
        (&["frobnicate"], "", Err("usage"), Err("usage")),
        (&["runs", "show"], "", Err("usage"), Err("usage")),
        (&["stop"], "", Ok, Ok),
    ];

    let paths = command_paths(&env).await;
    let words = |args: &[&str]| -> String {
        let mut path = String::new();
        for a in args {
            let longer = format!("{path} {a}").trim().to_owned();
            if !paths.contains(&longer) {
                break;
            }
            path = longer;
        }
        path
    };
    let covered: Vec<String> = cases.iter().map(|c| words(c.0)).collect();
    let leaves = paths.iter().filter(|p| !paths.iter().any(|q| q.starts_with(&format!("{p} "))));
    for path in leaves {
        let exempt = EXEMPT.contains(&path.as_str());
        assert!(
            exempt || covered.contains(path),
            "`pulse {path}` is not in the --json table; add what it answers with and without a daemon"
        );
    }

    let check = |args: &[&str],
                 want: Want,
                 (code, out, err): (i32, String, String),
                 daemon: &str| {
        let doc: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| {
            panic!(
                "{args:?} {daemon}: stdout is not one JSON document ({e}):\n{out}\nstderr: {err}"
            )
        });
        match want {
            Ok => assert!(
                code == 0 && doc.get("error").is_none(),
                "{args:?} {daemon}: exit {code}, {doc}\nstderr: {err}"
            ),
            Err(kind) => {
                let e = &doc["error"];
                assert_eq!(e["kind"], kind, "{args:?} {daemon}: {doc}");
                assert!(e["message"].as_str().is_some_and(|m| !m.is_empty()), "{args:?}: {doc}");
                let exit = match kind {
                    "usage" => 2,
                    "not_running" => 3,
                    _ => 1,
                };
                assert_eq!(code, exit, "{args:?} {daemon}: {doc}");
                assert_eq!(err, "", "{args:?} {daemon}: a failure is reported on stdout only");
            }
        }
        doc
    };

    for &(args, stdin, without, _) in cases {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        check(args, without, env.run_stdin(&full, stdin).await, "without a daemon");
    }
    let daemon = env.start().await;
    for &(args, stdin, _, with) in cases {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let doc = check(args, with, env.run_stdin(&full, stdin).await, "with a daemon");
        // What a script needs to act on the refusal travels with it.
        if let Err("stale") = with {
            assert_eq!(doc["error"]["code"], -32007, "{doc}");
            assert!(doc["error"]["data"]["current"].is_string(), "{doc}");
        }
        if let Err("config_invalid") = with {
            let first = &doc["error"]["data"]["errors"][0];
            assert!(first["file"].is_string() && first["line"].is_u64(), "{doc}");
        }
        if let Err("not_trusted") = with {
            assert!(doc["error"]["data"]["hash"].is_string(), "{doc}");
        }
        if args[0] == "ingest" && matches!(with, Err(_)) {
            let e = &doc["error"];
            assert_eq!(e["data"], serde_json::json!({ "line": 2, "accepted": 1, "duplicates": 0 }));
            assert!(e["message"].as_str().unwrap().contains("\"colour\""), "{doc}");
        }
        if let Err("not_found") = with {
            assert_eq!(doc["error"]["code"], -32002, "{doc}");
        }
    }
    // The daemon is gone after `stop` (the last case).
    let (code, _, _) = env.run(&["status"]).await;
    assert_eq!(code, 3);
    drop(daemon);
}

/// `pulse tail` shows an event before the runs it caused. They are committed together, but
/// events and runs reach a client on two subscriptions, each sending at most every 250 ms: an
/// event right after another waits for its subscription's next send while its run's goes out
/// at once.
#[tokio::test(flavor = "multi_thread")]
async fn tail_shows_an_event_before_its_runs() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let env = Env::new();
    env.write_config(
        "[[agent]]\nname = \"a\"\nadapter = \"command\"\ncommand = [\"true\"]\nallow = [\"ext.*\"]\n\n[[rule]]\nname = \"go\"\non = \"ext.ci.go\"\nrun = { agent = \"a\" }\n",
    );
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    let mut tail = env.pulse();
    tail.args(["--json", "tail"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut tail = tail.spawn().unwrap();
    let mut out = BufReader::new(tail.stdout.take().unwrap()).lines();
    let mut err = BufReader::new(tail.stderr.take().unwrap()).lines();
    within("tail to subscribe", err.next_line()).await.unwrap().unwrap();

    let mut earlier = Vec::new();
    for round in 0..5 {
        // The first event opens the events subscription's window; the second waits for it.
        for kind in ["warm", "go"] {
            let line = format!("{{\"type\":\"{kind}\",\"subject\":\"{round}\"}}\n");
            let (code, _, e) = env.run_stdin(&["ingest", "-s", "ci"], &line).await;
            assert_eq!(code, 0, "{e}");
        }
        let run = watch.run_in("this round's run", |r| !earlier.contains(&r.id)).await;
        earlier.push(run.id);
        let mut seen_event = false;
        loop {
            let line = within("a tail line", out.next_line()).await.unwrap().unwrap();
            let note = json(&line);
            let params = &note["params"];
            if note["method"] == "events.batch" {
                let ids = params["events"].as_array().unwrap();
                seen_event |= ids.iter().any(|e| e["id"] == run.event.to_string());
            }
            if note["method"] == "runs.changed" {
                let ours = params["runs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["id"] == run.id.to_string());
                if ours {
                    assert!(seen_event, "round {round}: run {} shown before its event", run.id);
                    break;
                }
            }
        }
    }
    drop(tail);
    daemon.stop().await;
}
