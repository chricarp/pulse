//! Hot reload, last-good config, write-back round trips, project trust, source restarts.

use pulse_contracts::config::RuleConfig;
use pulse_contracts::rpc::*;
use pulse_contracts::status::TrustState;
use pulse_daemon::client::{Client, ClientError};
use tokio::sync::mpsc;

use crate::support::*;

const BASE: &str = r#"# Pulse config for tests.
[[source]]
name = "fake"
kind = "schedule"
every = "1h"

[[agent]]
name = "sh"
adapter = "command"
command = ["true"]
allow = ["ext.*"]

# Reacts to CI.
[[rule]]
name = "ci"
on = "ext.ci.*"          # any CI event
run = { agent = "sh" }
"#;

/// Waits for `state.changed` notifications about `topic` until `holds` is true. A notification
/// from an earlier change may still be in flight, so one is not enough.
async fn until_state<F, Fut>(
    notes: &mut mpsc::Receiver<Notification>,
    topic: StateTopic,
    holds: F,
) -> StateChanged
where
    F: Fn(StateChanged) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    within(&format!("state.changed with {topic:?}"), async {
        loop {
            if let Notification::StateChanged(s) = notes.recv().await.unwrap() {
                if s.topics.contains(&topic) && holds(s.clone()).await {
                    return s;
                }
            }
        }
    })
    .await
}

async fn rule_names(client: &Client) -> Vec<String> {
    let list =
        client.call::<method::RulesList>(RulesListParams { include_archived: true }).await.unwrap();
    list.rules.into_iter().map(|r| r.config.name).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_edits_reload_and_broken_files_keep_the_last_good_config() {
    let mut t = TestDaemon::start(BASE).await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();
    assert_eq!(rule_names(&client).await, ["ci"]);

    t.write_config(&format!(
        "{BASE}\n[[rule]]\nname = \"second\"\non = \"ext.*\"\nrun = {{ agent = \"sh\" }}\n"
    ));
    until_state(&mut notes, StateTopic::Rules, |_| async { rule_names(&client).await.len() == 2 })
        .await;
    assert_eq!(rule_names(&client).await, ["ci", "second"]);

    t.write_config(&format!("{BASE}\n[[rule]]\nname = \"broken\"\non = \"ext.*\"\ncooldown = \"soon\"\nrun = {{ agent = \"sh\" }}\n"));
    let state =
        until_state(&mut notes, StateTopic::Config, |s| async move { s.status.config_errors > 0 })
            .await;
    assert_eq!(state.status.config_errors, 1);
    let errors = client.call::<method::SystemConfigErrors>(Empty {}).await.unwrap().errors;
    assert!(errors[0].message.contains("duration"), "{errors:?}");
    assert!(errors[0].line.is_some());
    assert_eq!(rule_names(&client).await, ["ci", "second"], "the last good config stays in effect");

    t.write_config(BASE);
    let state = until_state(&mut notes, StateTopic::Rules, |_| async {
        rule_names(&client).await.len() == 1
    })
    .await;
    assert_eq!(state.status.config_errors, 0);
    assert_eq!(rule_names(&client).await, ["ci"]);

    // A changed source is restarted; an unchanged one is left alone.
    let built = t.sources.built().len();
    t.write_config(&BASE.replace("every = \"1h\"", "every = \"2h\""));
    until_state(&mut notes, StateTopic::Sources, |_| async { t.sources.built().len() > built })
        .await;
    assert_eq!(t.sources.built().len(), built + 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_through_the_daemon_rewrite_the_file_and_keep_comments() {
    let mut t = TestDaemon::start(BASE).await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();

    let info = client
        .call::<method::RulesSetEnabled>(SetEnabledParams { name: "ci".into(), enabled: false })
        .await
        .unwrap();
    assert!(!info.config.enabled);
    let text = t.read_config();
    assert_eq!(
        text,
        BASE.replace("run = { agent = \"sh\" }\n", "run = { agent = \"sh\" }\nenabled = false\n")
    );

    // A new rule from the UI is appended; the rest of the file is untouched.
    let rule: RuleConfig = toml::from_str(
        "name = \"deploys\"\non = \"ext.deploy.*\"\ncooldown = \"5m\"\nrun = { agent = \"sh\" }",
    )
    .unwrap();
    client.call::<method::RulesUpsert>(RuleUpsertParams { rule, project: None }).await.unwrap();
    let text = t.read_config();
    assert!(text.contains("# Reacts to CI.") && text.contains("# any CI event"), "{text}");
    assert!(text.ends_with("[[rule]]\nname = \"deploys\"\non = [\"ext.deploy.*\"]\ncooldown = \"5m\"\nrun = { agent = \"sh\" }\n"), "{text}");

    // The other direction: a hand edit of what the daemon wrote shows up through the API.
    t.write_config(&text.replace("cooldown = \"5m\"", "cooldown = \"7m\""));
    let seven = Some(pulse_contracts::Duration::from_mins(7));
    until_state(&mut notes, StateTopic::Rules, |_| async {
        let got =
            client.call::<method::RulesGet>(NameParams { name: "deploys".into() }).await.unwrap();
        got.config.cooldown == seven
    })
    .await;

    // Invalid definitions are refused with located errors, and the file is not touched.
    let before = t.read_config();
    let bad: RuleConfig =
        toml::from_str("name = \"bad\"\non = \"ext.*\"\nrun = { agent = \"ghost\" }").unwrap();
    let err = client
        .call::<method::RulesUpsert>(RuleUpsertParams { rule: bad, project: None })
        .await
        .unwrap_err();
    match err {
        ClientError::Rpc(e) => {
            assert_eq!(e.kind(), Some(ErrorCode::ConfigInvalid));
            assert!(e.message.contains("ghost"), "{}", e.message);
        }
        other => panic!("{other}"),
    }
    assert_eq!(t.read_config(), before);

    // Archive and reopen, delete.
    client.call::<method::RulesArchive>(NameParams { name: "deploys".into() }).await.unwrap();
    let active = client.call::<method::RulesList>(RulesListParams::default()).await.unwrap();
    assert!(active.rules.iter().all(|r| r.config.name != "deploys"));
    client.call::<method::RulesReopen>(NameParams { name: "deploys".into() }).await.unwrap();
    client.call::<method::RulesDelete>(NameParams { name: "deploys".into() }).await.unwrap();
    client
        .call::<method::RulesSetEnabled>(SetEnabledParams { name: "ci".into(), enabled: true })
        .await
        .unwrap();
    assert_eq!(t.read_config(), BASE, "disable, enable: the file is as it was");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_trust_is_bound_to_content() {
    let mut t = TestDaemon::start(BASE).await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();
    let project = t.user_home.join("repo");
    std::fs::create_dir_all(&project).unwrap();
    let file = project.join("pulse.toml");
    let text = "[[rule]]\nname = \"build\"\non = \"ext.ci.*\"\nrun = { agent = \"sh\" }\n";
    std::fs::write(&file, text).unwrap();
    assert_eq!(rule_names(&client).await, ["ci"], "an untrusted project is inert");

    let dir = project.display().to_string();
    let p = client
        .call::<method::ProjectsTrust>(ProjectTrustParams { dir: dir.clone(), name: None })
        .await
        .unwrap();
    assert_eq!((p.name.as_str(), p.state), ("repo", TrustState::Trusted));
    assert_eq!(rule_names(&client).await, ["ci", "repo/build"]);

    // A `git pull` that changes the file: inert again until re-trusted.
    std::fs::write(&file, text.replace("ext.ci.*", "ext.*")).unwrap();
    until_state(&mut notes, StateTopic::Projects, |_| async {
        let list = client.call::<method::ProjectsList>(Empty {}).await.unwrap();
        list.projects[0].state == TrustState::Changed
    })
    .await;
    assert_eq!(rule_names(&client).await, ["ci"]);

    // `~` is the user's home, as everywhere else (the UI suggests `~/code/my-repo`).
    client
        .call::<method::ProjectsTrust>(ProjectTrustParams { dir: "~/repo".into(), name: None })
        .await
        .unwrap();
    assert_eq!(rule_names(&client).await, ["ci", "repo/build"]);

    // Editing a project rule through the daemon keeps the project trusted.
    client
        .call::<method::RulesSetEnabled>(SetEnabledParams {
            name: "repo/build".into(),
            enabled: false,
        })
        .await
        .unwrap();
    assert!(std::fs::read_to_string(&file).unwrap().contains("enabled = false"));
    let list = client.call::<method::ProjectsList>(Empty {}).await.unwrap();
    assert_eq!(list.projects[0].state, TrustState::Trusted);

    let p = client.call::<method::ProjectsUntrust>(ProjectDirParams { dir }).await.unwrap();
    assert_eq!(p.state, TrustState::Untrusted);
    assert_eq!(rule_names(&client).await, ["ci"]);
    t.stop().await;
}
