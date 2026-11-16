//! The other built-in sources through the daemon, once each: git (a commit and a branch
//! switch), a schedule, and a calendar event starting from a local `.ics` file.

use std::path::Path;

use pulse_contracts::status::SourceState;
use pulse_contracts::{RunStatus, Timestamp};

use crate::support::*;

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=E2E",
            "-c",
            "user.email=e2e@example.invalid",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `20261116T094107Z` for an ICS file.
fn ics_time(t: Timestamp) -> String {
    let s = t.to_string(); // 2026-11-16T09:41:07.123Z
    format!("{}Z", s[..19].replace(['-', ':'], ""))
}

#[tokio::test(flavor = "multi_thread")]
async fn git_schedule_and_calendar_sources_fire_rules() {
    let env = Env::new();
    let repo = env.mkdir("repo");
    git(&repo, &["init", "--quiet"]);
    std::fs::write(repo.join("a.txt"), "a").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "first"]);
    let now = Timestamp::from_system_time(std::time::SystemTime::now());
    let start = now.saturating_add(pulse_contracts::Duration::from_mins(2));
    let end = now.saturating_add(pulse_contracts::Duration::from_mins(32));
    std::fs::write(
        env.user_path("calendar.ics"),
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//pulse e2e//EN\r\nBEGIN:VEVENT\r\nUID:standup-1@e2e\r\n\
             DTSTAMP:20261116T000000Z\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:Standup\r\nLOCATION:Room 4\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n",
            ics_time(start),
            ics_time(end)
        ),
    )
    .unwrap();
    env.write_config(
        r#"
[[source]]
name = "repo"
kind = "git"
path = "~/repo"

[[source]]
name = "tick"
kind = "schedule"
every = "1s"

[[source]]
name = "work"
kind = "calendar"
path = "~/calendar.ics"

[[agent]]
name = "quick"
adapter = "command"
command = ["true"]
allow = ["git.*:~/repo/**", "schedule.*", "calendar.*"]
max_concurrent = 4

[[rule]]
name = "committed"
on = "git.commit.created"
run = { agent = "quick" }

[[rule]]
name = "switched"
on = "git.branch.switched"
where = { to = "feature" }
run = { agent = "quick" }

[[rule]]
name = "ticked"
on = "schedule.fired"
where = { source = "tick" }
run = { agent = "quick" }

[[rule]]
name = "meeting"
on = "calendar.event.starting"
where = { title = "Standup" }
run = { agent = "quick" }
"#,
    );
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.sources(SourceState::Healthy).await;

    std::fs::write(repo.join("b.txt"), "b").unwrap();
    git(&repo, &["add", "b.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "second: add b"]);
    git(&repo, &["checkout", "--quiet", "-b", "feature"]);

    for rule in ["committed", "switched", "ticked", "meeting"] {
        let run = watch.run_in(rule, |r| r.rule.as_deref() == Some(rule)).await;
        assert_eq!(watch.run_terminal(run.id).await.status, RunStatus::Succeeded, "{rule}");
        let event = watch.detail(run.event).await.event;
        match rule {
            "committed" => {
                assert_eq!(event.data["message"], "second: add b");
                assert_eq!(event.data["branch"], "main");
                assert_eq!(event.subject.as_deref(), Some(repo.display().to_string().as_str()));
            }
            "switched" => assert_eq!(
                (&event.data["from"], &event.data["to"]),
                (&"main".into(), &"feature".into())
            ),
            "ticked" => assert_eq!(event.type_.as_str(), "schedule.fired"),
            "meeting" => {
                assert_eq!(event.data["location"], "Room 4");
                assert_eq!(event.data["uid"], "standup-1@e2e");
            }
            _ => unreachable!(),
        }
    }
    // The first commit predates the source: history is not replayed.
    let commits = watch
        .query(pulse_contracts::rpc::EventFilter {
            types: vec!["git.commit.created".parse().unwrap()],
            ..Default::default()
        })
        .await;
    assert_eq!(commits.len(), 1);
    daemon.stop().await;
}
