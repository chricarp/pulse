//! Hot reload, last-good config, write-back round trips, project trust, source restarts.

use pulse_contracts::config::RuleConfig;
use pulse_contracts::rpc::*;
use pulse_contracts::status::{SourceState, TrustState};
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
    let edited = BASE.replace("every = \"1h\"", "every = \"2h\"");
    t.write_config(&edited);
    until_state(&mut notes, StateTopic::Sources, |_| async { t.sources.built().len() > built })
        .await;
    assert_eq!(t.sources.built().len(), built + 1);

    // A source that ended on its own is failed and stays failed; restarting it is the way back
    // without touching the file.
    let health = |name: &'static str| {
        let client = &client;
        async move {
            let list = client.call::<method::SourcesList>(Empty {}).await.unwrap();
            list.sources.into_iter().find(|s| s.config.name == name).unwrap().health
        }
    };
    t.sources.end("fake");
    until_state(&mut notes, StateTopic::Sources, |_| async {
        health("fake").await.state == SourceState::Failed
    })
    .await;
    assert!(health("fake").await.message.unwrap().contains("stopped by itself"));
    let restart =
        |name: &str| client.call::<method::SourcesRestart>(NameParams { name: name.into() });
    let s = restart("fake").await.unwrap();
    assert_ne!(s.health.state, SourceState::Failed, "{s:?}");
    assert_eq!(t.sources.built().len(), built + 2);
    // A healthy source restarts too, and the old instance has stopped before the new one is
    // built, here as on a reload.
    restart("fake").await.unwrap();
    assert_eq!(t.sources.built().len(), built + 3);
    assert_eq!(t.sources.overlaps(), Vec::<String>::new());
    assert_eq!(t.read_config(), edited, "a restart does not touch the file");
    // A disabled source is not started behind the user's back, and an unknown one is not found.
    client
        .call::<method::SourcesSetEnabled>(SetEnabledParams { name: "fake".into(), enabled: false })
        .await
        .unwrap();
    let err = restart("fake").await.unwrap_err();
    assert!(err.to_string().contains("disabled; enable it"), "{err}");
    assert_eq!(health("fake").await.state, SourceState::Disabled);
    let err = restart("nope").await.unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
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
    client
        .call::<method::RulesUpsert>(RuleUpsertParams {
            rule,
            project: None,
            expected: Expected::Absent,
        })
        .await
        .unwrap();
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
        .call::<method::RulesUpsert>(RuleUpsertParams {
            rule: bad,
            project: None,
            expected: Expected::Absent,
        })
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
    client
        .call::<method::RulesDelete>(DeleteParams {
            name: "deploys".into(),
            expected: Expected::Any,
        })
        .await
        .unwrap();
    client
        .call::<method::RulesSetEnabled>(SetEnabledParams { name: "ci".into(), enabled: true })
        .await
        .unwrap();
    assert_eq!(t.read_config(), BASE, "disable, enable: the file is as it was");
    t.stop().await;
}

fn stale(err: ClientError) -> Stale {
    match err {
        ClientError::Rpc(e) if e.kind() == Some(ErrorCode::Stale) => {
            serde_json::from_value(e.data.clone().unwrap()).unwrap()
        }
        other => panic!("expected a stale error, got {other}"),
    }
}

async fn rule(client: &Client, name: &str) -> RuleInfo {
    client.call::<method::RulesGet>(NameParams { name: name.into() }).await.unwrap()
}

/// An editor opened on a definition must not silently undo a change made elsewhere: every
/// write names what it was based on, and toggles change one field of the current definition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_are_refused_when_the_definition_changed_since_it_was_read() {
    let mut t = TestDaemon::start(BASE).await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();

    // The editor opens on `ci`; meanwhile `pulse rules disable ci` runs.
    let opened = rule(&client, "ci").await;
    let disabled = client
        .call::<method::RulesSetEnabled>(SetEnabledParams { name: "ci".into(), enabled: false })
        .await
        .unwrap();
    assert_ne!(disabled.revision, opened.revision);
    // Saving the untouched form is refused, with the current revision; the rule stays disabled.
    let save = |rule: RuleConfig, revision: &str| RuleUpsertParams {
        rule,
        project: None,
        expected: Expected::Revision { revision: revision.to_owned() },
    };
    let err = client
        .call::<method::RulesUpsert>(save(opened.config.clone(), &opened.revision))
        .await
        .unwrap_err();
    assert_eq!(stale(err).current.as_deref(), Some(disabled.revision.as_str()));
    assert!(t.read_config().contains("enabled = false"));
    // Overwriting deliberately means naming the revision the user was shown in the conflict.
    let overwritten = client
        .call::<method::RulesUpsert>(save(opened.config.clone(), &disabled.revision))
        .await
        .unwrap();
    assert!(overwritten.config.enabled);
    assert_eq!(overwritten.revision, opened.revision, "a revision is the content, not a counter");

    // Creating a name that exists is refused; so is deleting what changed or is gone.
    let err = client
        .call::<method::RulesUpsert>(RuleUpsertParams {
            rule: opened.config.clone(),
            project: None,
            expected: Expected::Absent,
        })
        .await
        .unwrap_err();
    assert_eq!(stale(err).current.as_deref(), Some(opened.revision.as_str()));
    let delete = |revision: &str| DeleteParams {
        name: "ci".into(),
        expected: Expected::Revision { revision: revision.to_owned() },
    };
    let err = client.call::<method::RulesDelete>(delete(&disabled.revision)).await.unwrap_err();
    assert_eq!(stale(err).current.as_deref(), Some(opened.revision.as_str()));

    // A hand edit the daemon has not reloaded yet: a toggle keeps it, because it changes one
    // field of the file as it is, not of the cached copy.
    t.write_config(&BASE.replace("on = \"ext.ci.*\"", "on = \"ext.build.*\"\nsupervised = true"));
    client
        .call::<method::RulesSetEnabled>(SetEnabledParams { name: "ci".into(), enabled: false })
        .await
        .unwrap();
    let text = t.read_config();
    assert!(text.contains("supervised = true") && text.contains("enabled = false"), "{text}");
    until_state(&mut notes, StateTopic::Rules, |_| async {
        rule(&client, "ci").await.config.supervised
    })
    .await;
    // The same for sources.
    t.write_config(&t.read_config().replace("every = \"1h\"", "every = \"3h\""));
    let s = client
        .call::<method::SourcesSetEnabled>(SetEnabledParams { name: "fake".into(), enabled: false })
        .await
        .unwrap();
    assert!(!s.config.enabled);
    assert!(t.read_config().contains("every = \"3h\""));

    // A comment is not a change: the revision is the definition's meaning.
    let before = rule(&client, "ci").await.revision;
    t.write_config(&t.read_config().replace("# Reacts to CI.", "# Reacts to builds."));
    let after = client
        .call::<method::RulesUpsert>(save(rule(&client, "ci").await.config, &before))
        .await
        .unwrap();
    assert_eq!(after.revision, before);

    // Removed while an editor had it open.
    let current = rule(&client, "ci").await.revision;
    client.call::<method::RulesDelete>(delete(&current)).await.unwrap();
    let err = client.call::<method::RulesDelete>(delete(&current)).await.unwrap_err();
    assert_eq!(stale(err).current, None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_trust_is_bound_to_the_reviewed_content() {
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
    let review = |dir: &str| {
        client.call::<method::ProjectsReview>(ProjectReviewParams { dir: dir.into(), name: None })
    };
    let trust = |dir: &str, hash: &str| {
        client.call::<method::ProjectsTrust>(ProjectTrustParams {
            dir: dir.into(),
            name: None,
            hash: hash.into(),
        })
    };
    let r = review(&dir).await.unwrap();
    assert_eq!((r.name.as_str(), r.state, r.text.as_str()), ("repo", TrustState::Untrusted, text));
    assert_eq!(r.rules.len(), 1);
    assert_eq!(r.changes, None);

    // The file is replaced while the user reads the review: trusting what they read fails, and
    // nothing from the new content is loaded.
    let swapped = text.replace("ext.ci.*", "ext.*");
    std::fs::write(&file, &swapped).unwrap();
    let err = trust(&dir, &r.hash).await.unwrap_err();
    let current = stale(err).current.unwrap();
    assert_ne!(current, r.hash);
    assert_eq!(rule_names(&client).await, ["ci"]);
    std::fs::write(&file, text).unwrap();
    let p = trust(&dir, &r.hash).await.unwrap();
    assert_eq!((p.name.as_str(), p.state), ("repo", TrustState::Trusted));
    assert_eq!(rule_names(&client).await, ["ci", "repo/build"]);

    // A `git pull` that changes the file: inert again until re-trusted, and the review says
    // what changed.
    std::fs::write(&file, &swapped).unwrap();
    until_state(&mut notes, StateTopic::Projects, |_| async {
        let list = client.call::<method::ProjectsList>(Empty {}).await.unwrap();
        list.projects[0].state == TrustState::Changed
    })
    .await;
    assert_eq!(rule_names(&client).await, ["ci"]);
    let r = review("~/repo").await.unwrap();
    assert_eq!(r.state, TrustState::Changed);
    assert_eq!(
        r.changes,
        Some(vec![DefinitionChange {
            kind: DefinitionKind::Rule,
            name: "build".into(),
            change: ChangeKind::Changed { fields: vec!["on".into()] },
        }])
    );

    // `~` is the user's home, as everywhere else (the UI suggests `~/code/my-repo`).
    trust("~/repo", &r.hash).await.unwrap();
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

    // But not when the file changed underneath: the text that would be edited is the text
    // whose hash is checked, so write-back never trusts content nobody reviewed.
    let pulled = std::fs::read_to_string(&file).unwrap().replace("ext.*", "ext.deploy.*");
    std::fs::write(&file, &pulled).unwrap();
    let err = client
        .call::<method::RulesSetEnabled>(SetEnabledParams {
            name: "repo/build".into(),
            enabled: true,
        })
        .await
        .unwrap_err();
    assert!(matches!(&err, ClientError::Rpc(e) if e.kind() == Some(ErrorCode::Conflict)), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), pulled);

    let p = client.call::<method::ProjectsUntrust>(ProjectDirParams { dir }).await.unwrap();
    assert_eq!(p.state, TrustState::Untrusted);
    assert_eq!(rule_names(&client).await, ["ci"]);

    // A project's `pulse.toml` that is a symbolic link (to anywhere: someone who can write the
    // repository chose where) is read and trusted for its content, but never written through.
    let linked = t.user_home.join("linked");
    let target = t.user_home.join("elsewhere.toml");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(&target, text).unwrap();
    std::os::unix::fs::symlink(&target, linked.join("pulse.toml")).unwrap();
    let dir = linked.display().to_string();
    let r = review(&dir).await.unwrap();
    trust(&dir, &r.hash).await.unwrap();
    assert_eq!(rule_names(&client).await, ["ci", "linked/build"]);
    let err = client
        .call::<method::RulesSetEnabled>(SetEnabledParams {
            name: "linked/build".into(),
            enabled: false,
        })
        .await
        .unwrap_err();
    assert!(matches!(&err, ClientError::Rpc(e) if e.kind() == Some(ErrorCode::Conflict)), "{err}");
    assert!(
        err.to_string().contains("symbolic link") && err.to_string().contains("by hand"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), text, "the link's target is untouched");
    assert!(std::fs::symlink_metadata(linked.join("pulse.toml")).unwrap().is_symlink());
    let list = client.call::<method::ProjectsList>(Empty {}).await.unwrap();
    assert!(list.projects.iter().all(|p| p.state == TrustState::Trusted || p.name == "repo"));
    t.stop().await;
}
