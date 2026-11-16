//! The two shipped templates, as shipped, with `fake-claude` standing in for `claude` through the
//! `claude-code` adapter. Only durations are shortened, and the agent's `program` and `env` are
//! added; `replace` fails if the template text it edits has changed.

use pulse_contracts::rpc::{EventFilter, method};
use pulse_contracts::status::SourceState;
use pulse_contracts::{DeliveryOutcome, OutputKind, RunStatus};

use crate::support::*;

fn fake_claude(text: &str, env: &[(&str, String)]) -> String {
    let env: Vec<String> = env.iter().map(|(k, v)| format!("{k} = {v:?}")).collect();
    replace(
        text,
        "adapter = \"claude-code\"\n",
        &format!(
            "adapter = \"claude-code\"\nprogram = {:?}\nenv = {{ {} }}\n",
            fixture("fake-claude"),
            env.join(", ")
        ),
    )
}

/// Definition of done: a PDF dropped into the watched folder runs the study agent, and its
/// output reaches a subscriber while the run is still running.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_pdf_runs_the_study_agent_and_output_streams_while_it_runs() {
    let env = Env::new();
    let university = env.mkdir("University/Architettura");
    let stream = Fifo::new(env.dir.path(), "stream");
    let prompt_file = env.dir.path().join("prompt.txt");
    let config = template("lecture-notes");
    let config = replace(&config, "debounce = \"10s\"", "debounce = \"200ms\"");
    let config = replace(
        &config,
        "path = \"~/University\"\n",
        "path = \"~/University\"\nquiet = \"100ms\"\n",
    );
    let config = fake_claude(
        &config,
        &[
            ("FAKE_AGENT_STREAM", stream.path.display().to_string()),
            ("FAKE_AGENT_STDIN_FILE", prompt_file.display().to_string()),
        ],
    );
    env.write_config(&config);
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.sources(SourceState::Healthy).await;

    let pdf = university.join("lecture-07.pdf");
    std::fs::write(&pdf, b"%PDF-1.4\n% not really a lecture\n").unwrap();
    let pdf = pdf.display().to_string();

    let run = watch
        .run_in("a run of lecture-notes", |r| r.rule.as_deref() == Some("lecture-notes"))
        .await;
    watch.follow_output(run.id).await;
    let mut stream = stream;
    stream.open().await;
    let lines = std::fs::read_to_string(fixtures().join("claude-code/answer.jsonl")).unwrap();
    let mut lines = lines.lines();
    stream.write(&format!("{}\n", lines.next().unwrap()));
    // The agent cannot end before the stream closes, so output seen now was seen live.
    watch
        .until("the first output", |w| {
            w.output
                .get(&run.id)
                .is_some_and(|o| o.iter().any(|c| c.output.kind == OutputKind::System))
                .then_some(())
        })
        .await;
    assert_eq!(watch.get_run(run.id).await.status, RunStatus::Running);
    for l in lines {
        stream.write(&format!("{l}\n"));
    }
    stream.close();
    let ended = watch.run_terminal(run.id).await;
    assert_eq!(ended.status, RunStatus::Succeeded);
    watch.until("the output to finish", |w| w.output_done.get(&run.id).copied()).await;
    let kinds: Vec<OutputKind> = watch.output[&run.id].iter().map(|c| c.output.kind).collect();
    assert!(kinds.contains(&OutputKind::Result), "{kinds:?}");

    // The invocation Pulse built, and the prompt as the agent received it.
    let full = watch.get_run(run.id).await;
    let inv = full.invocation.unwrap();
    assert_eq!(inv.cwd, env.user_path("University").display().to_string());
    for flag in ["--permission-mode=dontAsk", "--allowedTools=Read", "--allowedTools=Edit(./**)"] {
        assert!(inv.args.iter().any(|a| a == flag), "{flag} in {:?}", inv.args);
    }
    let prompt = std::fs::read_to_string(&prompt_file).unwrap();
    assert!(prompt.starts_with(&format!("A new lecture PDF was saved at {pdf}.")), "{prompt}");
    assert!(prompt.contains("untrusted data"), "{prompt}");

    // One event, debounced, then fired once.
    let detail = watch.detail(full.event).await;
    assert_eq!(detail.event.type_.as_str(), "fs.file.created");
    assert_eq!(detail.event.subject.as_deref(), Some(pdf.as_str()));
    let outcomes: Vec<&str> = detail.deliveries.iter().map(|d| d.outcome.kind_str()).collect();
    assert_eq!(outcomes, ["debounced", "fired"]);
    assert_eq!(watch.all_runs().await.len(), 1);
    daemon.stop().await;
}

/// Definition of done: `pulse run -- <failing command>` runs the diagnosis rule; a command that
/// succeeds does not.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_wrapped_command_runs_the_diagnosis_and_a_passing_one_does_not() {
    let env = Env::new();
    let project = env.mkdir("code/app");
    let prompt_file = env.dir.path().join("prompt.txt");
    let config = fake_claude(
        &template("build-failure"),
        &[("FAKE_AGENT_STDIN_FILE", prompt_file.display().to_string())],
    );
    env.write_config(&config);
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;

    let (code, out, _) = env
        .run_in(
            &["run", "--", "sh", "-c", "echo 'error[E0308]: mismatched types'; exit 3"],
            &[],
            Some(&project),
        )
        .await;
    assert_eq!(code, 3);
    assert_eq!(out.trim(), "error[E0308]: mismatched types", "the command's output passes through");
    let run = watch.run_in("a diagnosis", |r| r.rule.as_deref() == Some("build-failed")).await;
    assert_eq!(watch.run_terminal(run.id).await.status, RunStatus::Succeeded);
    let full = watch.get_run(run.id).await;
    let prompt = std::fs::read_to_string(&prompt_file).unwrap();
    // The instruction names the event and inlines only a path; the command line and its
    // output, which the command's arguments and output control, stay in the data block.
    let (instruction, data) =
        prompt.split_once("The event that triggered this run follows").expect("a data block");
    assert!(instruction.contains(&project.display().to_string()), "{instruction}");
    assert!(!instruction.contains("E0308"), "the command line is not inlined: {instruction}");
    assert!(data.contains("mismatched types"), "the output tail is in the data block: {data}");
    let event = watch.detail(full.event).await.event;
    assert_eq!(event.data["exit_code"], 3);
    assert_eq!(event.data["cwd"], project.display().to_string());

    let (code, _, _) = env.run_in(&["run", "--", "true"], &[], Some(&project)).await;
    assert_eq!(code, 0);
    // `pulse run` returns after the event is committed, with its routing decisions.
    let exited = watch
        .query(EventFilter { types: vec!["process.exited".parse().unwrap()], ..Default::default() })
        .await;
    assert_eq!(exited.len(), 2);
    let passing = &exited[1];
    assert_eq!(passing.data["success"], true);
    let detail = watch.detail(passing.id).await;
    assert!(detail.deliveries.is_empty(), "{:?}", detail.deliveries);
    assert_eq!(watch.all_runs().await.len(), 1);

    // Outside ~/code the rule does not match at all.
    let elsewhere = env.mkdir("elsewhere");
    let (code, _, _) = env.run_in(&["run", "--", "false"], &[], Some(&elsewhere)).await;
    assert_eq!(code, 1);
    let page = watch.client.call::<method::EventsQuery>(Default::default()).await.unwrap();
    let newest = &page.page.events[0];
    assert_eq!(newest.data["success"], false);
    let detail = watch.detail(newest.id).await;
    assert!(
        detail.deliveries.iter().all(|d| !matches!(d.outcome, DeliveryOutcome::Fired)),
        "{:?}",
        detail.deliveries
    );
    daemon.stop().await;
}
