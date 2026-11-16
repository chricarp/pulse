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

    // Fixing the agent clears it.
    let (code, _, err) = env
        .run_stdin(
            &["agents", "add", "--file", "-"],
            "name = \"notes\"\nadapter = \"command\"\ncommand = [\"true\"]\nallow = [\"fs.*:~/inbox/**\"]\n",
        )
        .await;
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
