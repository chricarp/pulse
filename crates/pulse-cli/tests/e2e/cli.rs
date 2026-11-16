//! The CLI as scripts and people use it: exit codes, `--json` shapes, `pulse run` passing its
//! command's exit through, and adding definitions without opening the config file.

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
    for args in [&["status"][..], &["runs", "list"], &["--json", "rules", "list"], &["tail"]] {
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
    let hash = json(&out)["hash"].as_str().unwrap().to_owned();

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
