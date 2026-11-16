use std::path::{Path, PathBuf};

use super::*;
use crate::config::{DebounceBy, FsSourceSettings, StdinMode};
use crate::time::Duration;

const HOME: &str = "/home/me";

fn user() -> ConfigOrigin {
    ConfigOrigin::User { path: PathBuf::from("/home/me/.pulse/pulse.toml") }
}

fn project(name: &str) -> ConfigOrigin {
    ConfigOrigin::Project {
        name: name.into(),
        dir: PathBuf::from(format!("/home/me/code/{name}")),
        path: PathBuf::from(format!("/home/me/code/{name}/pulse.toml")),
    }
}

fn parse(text: &str) -> Result<ConfigFile, ConfigErrors> {
    parse_config_file(text, &user(), Path::new(HOME))
}

const EXAMPLE: &str = r#"
[[source]]
name = "university"
kind = "fs"
path = "~/University"
ignore = ["**/.git/**"]
quiet = "3s"
burst_threshold = 500

[[agent]]
name    = "study"
adapter = "claude-code"
allow   = ["fs.*:~/University/**", "calendar.*"]
cwd     = "~/University"
timeout = "15m"
max_concurrent = 1

[[agent]]
name = "notify"
adapter = "command"
command = ["notify-send", "--", "{{subject}}"]

[[rule]]
name       = "new-lecture"
on         = "fs.file.created"
where      = { path = "~/University/**/*.pdf" }
debounce   = "10s"
cooldown   = "30s"
rate_limit = "10/hour"
supervised = false
enabled    = true
run        = { agent = "study", prompt = "Summarize {{subject}} and create flashcards next to it." }
"#;

#[test]
fn parses_the_documented_example() {
    let file = parse(EXAMPLE).unwrap();
    assert_eq!(file.sources.len(), 1);
    let SourceSettings::Fs(FsSourceSettings { path, ignore, recursive, .. }) =
        &file.sources[0].settings
    else {
        panic!("fs source")
    };
    assert_eq!((path.as_str(), ignore.len(), *recursive), ("~/University", 1, true));
    assert!(file.sources[0].enabled);
    let study = &file.agents[0];
    assert_eq!(study.kind(), AgentKind::ClaudeCode);
    assert_eq!(study.allow.len(), 2);
    assert_eq!(study.max_queued, 8);
    let AdapterSettings::Command(cmd) = &file.agents[1].adapter else { panic!("command agent") };
    assert_eq!(cmd.stdin, StdinMode::Prompt);
    let rule = &file.rules[0];
    assert_eq!(rule.debounce, Some(Duration::from_secs(10)));
    assert_eq!(rule.debounce_by, DebounceBy::Rule);
    assert_eq!(rule.rate_limit.map(|r| r.count), Some(10));
    assert!(rule.enabled && !rule.archived && !rule.supervised);

    // The wire form round-trips through JSON (what rules.upsert and friends carry).
    let json = serde_json::to_value(&file.rules[0]).unwrap();
    assert_eq!(json["on"], serde_json::json!(["fs.file.created"]));
    assert_eq!(serde_json::from_value::<RuleConfig>(json).unwrap(), file.rules[0]);
    let json = serde_json::to_value(&file.agents[0]).unwrap();
    assert_eq!(json["adapter"], "claude-code");
    assert_eq!(serde_json::from_value::<AgentConfig>(json).unwrap(), file.agents[0]);
    let json = serde_json::to_value(&file.sources[0]).unwrap();
    assert_eq!(json["kind"], "fs");
    assert_eq!(serde_json::from_value::<SourceConfig>(json).unwrap(), file.sources[0]);
}

#[test]
fn errors_name_the_file_line_item_and_field() {
    // (config text, substrings the rendered error must contain)
    let cases: &[(&str, &[&str])] = &[
        ("bogus = 1", &["pulse.toml:1:1", "unknown field `bogus`"]),
        (
            "[[rule]]\nname = \"r\"\non = \"fs.*\"\nrun = { agent = \"a\", prompt = \"p\" }\ncolour = \"red\"",
            &[":5:", "rule \"r\"", "unknown field `colour`"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"fs\"\npath = \"/x\"\nrecursiv = false",
            &["source \"s\"", "unknown field `recursiv`"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"ftp\"\npath = \"/x\"",
            &["source \"s\"", "unknown variant `ftp`"],
        ),
        (
            "[[agent]]\nname = \"a\"\nadapter = \"claude-code\"\nmodle = \"x\"",
            &["agent \"a\"", "unknown field `modle`"],
        ),
        (
            "[[rule]]\nname = \"r\"\non = \"fs.*\"\ndebounce = \"10 seconds\"\nrun = { agent = \"a\", prompt = \"p\" }",
            &[":4:", "rule \"r\"", "field `debounce`", "expected a duration like"],
        ),
        (
            "[[rule]]\nname = \"r\"\non = \"fs.*\"\nrate_limit = \"10/fortnight\"\nrun = { agent = \"a\", prompt = \"p\" }",
            &["rule \"r\"", "field `rate_limit`", "expected a rate like"],
        ),
        (
            "[[rule]]\nname = \"r\"\non = \"FS.*\"\nrun = { agent = \"a\", prompt = \"p\" }",
            &["rule \"r\"", "field `on`", "not a valid event type glob"],
        ),
        (
            "[[rule]]\nname = \"r\"\non = \"fs.*\"\nrun = { agent = \"a\", promt = \"p\" }",
            &["rule \"r\"", "unknown field `promt`"],
        ),
        (
            "[[rule]]\nname = \"r\"\non = \"fs.*\"\nwhere = { path = \"~/U/../etc/**\" }\nrun = { agent = \"a\", prompt = \"p\" }",
            &[":4", "rule \"r\"", "field `where.path`", "'..'"],
        ),
        (
            "[[rule]]\nname = \"r\"\non = \"fs.*\"\nrun = { agent = \"a\", prompt = \"Hi {{subject\" }",
            &[":4", "field `run.prompt`", "unclosed"],
        ),
        (
            "[[agent]]\nname = \"a\"\nadapter = \"claude-code\"\nallow = [\"fs.*:University/**\"]",
            &["agent \"a\"", "invalid scope"],
        ),
        (
            "[[agent]]\nname = \"a\"\nadapter = \"claude-code\"\nallow = [\"fs.*:~/U/**x\"]",
            &[":4", "agent \"a\"", "field `allow[0]`", "whole path segment"],
        ),
        (
            "[[agent]]\nname = \"a\"\nadapter = \"command\"\ncommand = [\"{{subject}}\"]",
            &["field `command[0]`", "cannot contain placeholders"],
        ),
        (
            "[[agent]]\nname = \"a\"\nadapter = \"codex\"\nenv = { PULSE_RUN_ID = \"x\" }",
            &["field `env`", "reserved"],
        ),
        (
            "[[agent]]\nname = \"Study\"\nadapter = \"codex\"",
            &["agent \"Study\"", "field `name`", "invalid name"],
        ),
        (
            "[[agent]]\nname = \"a\"\nadapter = \"codex\"\n[[agent]]\nname = \"a\"\nadapter = \"codex\"",
            &["agent \"a\"", "same name"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"schedule\"",
            &["source \"s\"", "exactly one of `cron` and `every`"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"schedule\"\ncron = \"every day\"",
            &["field `cron`", "five fields"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"calendar\"\nurl = \"ftp://x\"",
            &["field `url`", "http(s) URL"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"git\"\npath = \"code/x\"",
            &["field `path`", "absolute"],
        ),
        // Values a source refuses when it starts are config errors, with the field's line.
        (
            "[[source]]\nname = \"s\"\nkind = \"calendar\"\npath = \"/c.ics\"\nlead = \"366d\"",
            &[":5", "source \"s\"", "field `lead`", "at most 365d"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"calendar\"\nurl = \"https://x/c.ics\"\nrefresh = \"400d\"",
            &[":5", "field `refresh`", "at most 365d"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"schedule\"\nevery = \"3651d\"",
            &[":4", "field `every`", "at most 3650d"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"schedule\"\ncron = \"*/90 * * * *\"",
            &[":4", "field `cron`", "the step must be at most 60", "use `every`"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"schedule\"\ncron = \"61 * * * *\"",
            &["field `cron`", "61 is outside 0-59"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"schedule\"\ncron = \"0 0 30 2 *\"",
            &["field `cron`", "never fires"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"schedule\"\ncron = \"0 9 * * *\"\ntimezone = \"Mars/Olympus\"",
            &[":5", "field `timezone`", "unknown time zone"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"fs\"\npath = \"/x\"\nignore = [\"*.log\", \"build/../secret\"]",
            &[":5", "field `ignore[1]`", "'..'"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"fs\"\npath = \"/x\"\nignore = [\"~root/x\"]",
            &["field `ignore[0]`", "~root"],
        ),
        (
            "[[source]]\nname = \"s\"\nkind = \"fs\"\npath = \"/x\"\nignore = [\"/\"]",
            &["field `ignore[0]`", "empty"],
        ),
        ("[limits]\nmax_chain_depth = 0", &["[limits]", "field `max_chain_depth`", "from 1 to 32"]),
        ("[limits]\nmax_chain_dept = 3", &["[limits]", "unknown field `max_chain_dept`"]),
    ];
    for (text, wants) in cases {
        let err = parse(text).expect_err(text).to_string();
        for want in *wants {
            assert!(err.contains(want), "for config:\n{text}\nexpected {want:?} in:\n{err}");
        }
    }
}

#[test]
fn project_files_are_restricted_and_resolve_relative_paths() {
    let home = Path::new(HOME);
    let text = "[daemon]\nport = 1\n[[source]]\nname = \"src\"\nkind = \"fs\"\npath = \"src\"";
    let err = parse_config_file(text, &project("pulse"), home).unwrap_err().to_string();
    assert!(err.contains("[daemon]") && err.contains("only allowed"), "{err}");
    let ok = "[[source]]\nname = \"src\"\nkind = \"fs\"\npath = \"src\"";
    assert!(parse_config_file(ok, &project("pulse"), home).is_ok());
    assert!(parse_config_file(ok, &user(), home).is_err(), "relative paths need a project base");
}

#[test]
fn resolve_prefixes_projects_and_finds_agents() {
    let home = Path::new(HOME);
    let user_file = parse_config_file(
        "[[agent]]\nname = \"study\"\nadapter = \"claude-code\"\n[[agent]]\nname = \"watcher\"\nadapter = \"mcp\"",
        &user(),
        home,
    )
    .unwrap();
    let proj_text = r#"
[[agent]]
name = "lint"
adapter = "command"
command = ["make", "lint"]

[[rule]]
name = "local"
on = "fs.*"
run = { agent = "lint" }

[[rule]]
name = "global"
on = "git.*"
run = { agent = "study", prompt = "Review {{subject}}" }
"#;
    let proj = parse_config_file(proj_text, &project("pulse"), home).unwrap();
    let config = resolve(
        Sourced { origin: user(), config: user_file.clone() },
        vec![Sourced { origin: project("pulse"), config: proj }],
    )
    .unwrap();
    let names: Vec<_> = config
        .rules
        .iter()
        .map(|r| (r.config.name.as_str(), r.config.run.agent.as_str()))
        .collect();
    assert_eq!(names, vec![("pulse/local", "pulse/lint"), ("pulse/global", "study")]);
    assert!(config.agent("pulse/lint").is_some());
    assert_eq!(config.limits, Limits::default());

    let bad = parse_config_file(
        "[[rule]]\nname = \"a\"\non = \"fs.*\"\nrun = { agent = \"watcher\", prompt = \"x\" }\n[[rule]]\nname = \"b\"\non = \"fs.*\"\nrun = { agent = \"study\" }\n[[rule]]\nname = \"c\"\non = \"fs.*\"\nrun = { agent = \"ghost\", prompt = \"x\" }",
        &project("p"),
        home,
    )
    .unwrap();
    let err = resolve(
        Sourced { origin: user(), config: user_file },
        vec![Sourced { origin: project("p"), config: bad }],
    )
    .unwrap_err()
    .to_string();
    for want in [
        "rule \"a\", field `run.agent`: agent \"watcher\" uses the mcp adapter",
        "rule \"b\", field `run.prompt`",
        "rule \"c\", field `run.agent`: no agent named \"ghost\"",
    ] {
        assert!(err.contains(want), "expected {want:?} in:\n{err}");
    }
}
