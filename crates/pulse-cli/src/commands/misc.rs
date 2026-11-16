//! `agents`, `sources`, `project`.

use std::path::PathBuf;
use std::process::ExitCode;

use pulse_contracts::config::{AgentConfig, ConfigOrigin, SourceConfig};
use pulse_contracts::rpc::*;
use pulse_contracts::status::{Project, TrustState};

use super::Res;
use crate::cli::{AgentsArgs, AgentsCmd, ProjectCmd, SourceKindArg, SourcesArgs, SourcesCmd};
use crate::{Ctx, Failure, print_json, print_line};

fn origin(o: &ConfigOrigin) -> String {
    match o {
        ConfigOrigin::User { .. } => String::new(),
        ConfigOrigin::Project { name, .. } => format!(" (project {name})"),
    }
}

/// `claude-code`, as written in config.
fn kind_name(kind: pulse_contracts::config::AgentKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

pub async fn agents(ctx: &Ctx, args: AgentsArgs) -> Res {
    let client = ctx.client().await?;
    match args.cmd.unwrap_or(AgentsCmd::List) {
        AgentsCmd::List => {
            let list = client.call::<method::AgentsList>(Empty {}).await?;
            if ctx.json {
                print_json(&list);
            } else if list.agents.is_empty() {
                print_line("No agents.");
            }
            for a in list.agents.iter().filter(|_| !ctx.json) {
                let scopes: Vec<String> = a.config.allow.iter().map(ToString::to_string).collect();
                print_line(&format!(
                    "{:<20} {:<12} running {} queued {}  allow [{}]{}",
                    a.config.name,
                    kind_name(a.config.kind()),
                    a.running,
                    a.queued,
                    scopes.join(", "),
                    origin(&a.origin)
                ));
            }
        }
        AgentsCmd::Show { name } => {
            let a = client.call::<method::AgentsGet>(NameParams { name }).await?;
            if ctx.json {
                print_json(&a);
            } else {
                print_line(&format!(
                    "# {} — running {}, queued {}{}",
                    a.config.name,
                    a.running,
                    a.queued,
                    origin(&a.origin)
                ));
                print_line(&toml::to_string_pretty(&a.config).unwrap_or_default());
            }
        }
        AgentsCmd::Add { file, project, replace } => {
            let agent: AgentConfig = super::read_definition(&file)?;
            let what = format!("an agent named {:?}", agent.name);
            let expected = super::add_expected(replace);
            let a = client
                .call::<method::AgentsUpsert>(AgentUpsertParams { agent, project, expected })
                .await
                .map_err(|e| super::exists_hint(e, &what))?;
            if ctx.json {
                print_json(&a);
            } else {
                print_line(&format!("Saved agent {}{}.", a.config.name, origin(&a.origin)));
            }
            super::warn_about(ctx, &client, &format!("agent {:?}", a.config.name)).await?;
        }
        AgentsCmd::Remove { name } => {
            let params = DeleteParams { name: name.clone(), expected: Expected::Any };
            client.call::<method::AgentsDelete>(params).await?;
            if ctx.json {
                print_json(&serde_json::json!({ "removed": name }));
            } else {
                print_line(&format!("Removed agent {name}."));
            }
        }
        AgentsCmd::McpCredentials { name } => {
            let c = client.call::<method::AgentsMcpCredentials>(NameParams { name }).await?;
            if ctx.json {
                print_json(&c);
            } else {
                print_line(&super::mcp::credentials_text(ctx, &c));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn sources(ctx: &Ctx, args: SourcesArgs) -> Res {
    let client = ctx.client().await?;
    match args.cmd.unwrap_or(SourcesCmd::List) {
        SourcesCmd::List => {}
        SourcesCmd::Remove { name } => {
            let params = DeleteParams { name: name.clone(), expected: Expected::Any };
            client.call::<method::SourcesDelete>(params).await?;
            if ctx.json {
                print_json(&serde_json::json!({ "removed": name }));
            } else {
                print_line(&format!("Removed source {name}."));
            }
            return Ok(ExitCode::SUCCESS);
        }
        SourcesCmd::Add {
            kind,
            name,
            target,
            cron,
            every,
            timezone,
            ignore,
            lead,
            project,
            replace,
        } => {
            let source = source_config(kind, name, target, cron, every, timezone, ignore, lead)?;
            let what = format!("a source named {:?}", source.name);
            let expected = super::add_expected(replace);
            let s = client
                .call::<method::SourcesUpsert>(SourceUpsertParams { source, project, expected })
                .await
                .map_err(|e| super::exists_hint(e, &what))?;
            if ctx.json {
                print_json(&s);
            } else {
                print_line(&format!(
                    "Saved source {} ({}){}. `pulse sources` shows its health.",
                    s.config.name,
                    s.config.kind().as_str(),
                    origin(&s.origin)
                ));
            }
            return Ok(ExitCode::SUCCESS);
        }
    }
    let list = client.call::<method::SourcesList>(Empty {}).await?;
    if ctx.json {
        print_json(&list);
        return Ok(ExitCode::SUCCESS);
    }
    if list.sources.is_empty() {
        print_line("No sources.");
    }
    for s in &list.sources {
        let h = &s.health;
        let state = format!("{:?}", h.state).to_lowercase();
        let mut line = format!(
            "{:<20} {:<9} {:<9} {} events{}",
            s.config.name,
            s.config.kind().as_str(),
            state,
            h.events_emitted,
            origin(&s.origin)
        );
        if let Some(m) = &h.message {
            line.push_str(&format!("  ({m})"));
        }
        print_line(&line);
        if let Some(e) = h.last_error.as_ref().filter(|e| h.message.as_ref() != Some(&e.message)) {
            print_line(&format!("    last problem {}: {}", e.at, e.message));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// A source definition from `sources add` flags, validated by the same deserializer as the
/// config file. A relative path is taken from the current directory, as a shell user expects.
#[allow(clippy::too_many_arguments)]
fn source_config(
    kind: SourceKindArg,
    name: String,
    target: Option<String>,
    cron: Option<String>,
    every: Option<String>,
    timezone: Option<String>,
    ignore: Vec<String>,
    lead: Option<String>,
) -> Result<SourceConfig, Failure> {
    let mut t = toml::Table::new();
    t.insert("name".into(), name.into());
    let kind_name = match kind {
        SourceKindArg::Fs => "fs",
        SourceKindArg::Git => "git",
        SourceKindArg::Schedule => "schedule",
        SourceKindArg::Calendar => "calendar",
    };
    t.insert("kind".into(), kind_name.into());
    let path = |p: String| -> Result<String, Failure> {
        if p.starts_with('/') || p.starts_with('~') { Ok(p) } else { absolute(PathBuf::from(p)) }
    };
    match (kind, target) {
        (SourceKindArg::Schedule, Some(t)) => {
            return Err(Failure::Message(format!("a schedule takes --cron or --every, not {t:?}")));
        }
        (SourceKindArg::Schedule, None) => {}
        (_, None) => {
            return Err(Failure::Message(format!("a {kind_name} source needs a path (or URL)")));
        }
        (SourceKindArg::Calendar, Some(u))
            if u.starts_with("http://") || u.starts_with("https://") =>
        {
            t.insert("url".into(), u.into());
        }
        (_, Some(p)) => {
            t.insert("path".into(), path(p)?.into());
        }
    }
    for (key, value) in [("cron", cron), ("every", every), ("timezone", timezone), ("lead", lead)] {
        if let Some(v) = value {
            t.insert(key.into(), v.into());
        }
    }
    if !ignore.is_empty() {
        t.insert("ignore".into(), ignore.into());
    }
    toml::Value::Table(t)
        .try_into()
        .map_err(|e: toml::de::Error| Failure::Message(e.message().to_owned()))
}

fn absolute(dir: PathBuf) -> Result<String, Failure> {
    let abs = if dir.is_absolute() { dir } else { std::env::current_dir()?.join(dir) };
    Ok(abs.display().to_string())
}

fn print_project(p: &Project) {
    let state = match p.state {
        TrustState::Trusted => "trusted",
        TrustState::Changed => "changed since trusted: inactive until `pulse project trust` again",
        TrustState::Missing => "pulse.toml is missing",
        TrustState::Untrusted => "not trusted",
    };
    print_line(&format!("{:<16} {}  {state}", p.name, p.dir));
}

/// A directory for a command line the user may paste.
fn shell_word(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~+".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// A project file's review: what it would load, then what changed. Every string in it comes
/// from the repository; `print_line` escapes terminal control characters.
fn print_review(r: &ProjectReview) {
    let state = match r.state {
        TrustState::Untrusted => "not trusted",
        TrustState::Trusted => "trusted, unchanged",
        TrustState::Changed => "changed since it was trusted",
        TrustState::Missing => "missing",
    };
    print_line(&format!("# {} ({state})", r.file));
    print_line(&format!("# definitions are named {}/…", r.name));
    for e in &r.errors {
        print_line(&format!("error: {e}"));
    }
    if let Some(changes) = &r.changes {
        print_line("");
        if changes.is_empty() {
            print_line(
                "Changes since it was trusted: none in its definitions (only formatting or comments).",
            );
        } else {
            print_line("Changes since it was trusted:");
        }
        for c in changes {
            let kind = match c.kind {
                DefinitionKind::Rule => "rule",
                DefinitionKind::Agent => "agent",
                DefinitionKind::Source => "source",
            };
            let what = match &c.change {
                ChangeKind::Added => "added".to_owned(),
                ChangeKind::Removed => "removed".to_owned(),
                ChangeKind::Changed { fields } => format!("changed: {}", fields.join(", ")),
            };
            print_line(&format!("  {kind} {}: {what}", c.name));
        }
    }
    let defs = [
        ("agent", r.agents.iter().map(toml::to_string_pretty).collect::<Vec<_>>()),
        ("rule", r.rules.iter().map(toml::to_string_pretty).collect()),
        ("source", r.sources.iter().map(toml::to_string_pretty).collect()),
    ];
    for (kind, items) in defs {
        for item in items {
            print_line("");
            print_line(&format!("# {kind}"));
            print_line(item.unwrap_or_default().trim_end());
        }
    }
    print_line("");
    print_line(&format!("# content hash {}", r.hash));
}

pub async fn project(ctx: &Ctx, cmd: ProjectCmd) -> Res {
    let client = ctx.client().await?;
    match cmd {
        ProjectCmd::List => {
            let list = client.call::<method::ProjectsList>(Empty {}).await?;
            if ctx.json {
                print_json(&list);
            } else if list.projects.is_empty() {
                print_line("No trusted projects.");
            } else {
                list.projects.iter().for_each(print_project);
            }
        }
        ProjectCmd::Trust { dir, name, hash: None } => {
            // Looking and trusting are two calls; binding the second to the hash the first
            // printed is what makes the user trust what they read, interactively or not.
            let dir = absolute(dir)?;
            let review = client
                .call::<method::ProjectsReview>(ProjectReviewParams { dir: dir.clone(), name })
                .await?;
            if ctx.json {
                print_json(&review);
            } else {
                print_review(&review);
                if review.errors.is_empty() {
                    let name_arg = format!(" --name {}", review.name);
                    crate::eprint_line(&format!(
                        "Nothing was trusted. To trust exactly this content:\n  pulse project trust {}{name_arg} --hash {}",
                        shell_word(&review.dir),
                        review.hash
                    ));
                } else {
                    crate::eprint_line("Nothing was trusted: the file has errors.");
                }
            }
            return Ok(ExitCode::FAILURE);
        }
        ProjectCmd::Trust { dir, name, hash: Some(hash) } => {
            let params = ProjectTrustParams { dir: absolute(dir)?, name, hash };
            let p = client.call::<method::ProjectsTrust>(params).await.map_err(|e| match e {
                pulse_daemon::client::ClientError::Rpc(err)
                    if err.kind() == Some(ErrorCode::Stale) =>
                {
                    Failure::Message(
                        "the file changed since that hash was printed; nothing was trusted. Run `pulse project trust` without --hash to review it again.".into(),
                    )
                }
                other => other.into(),
            })?;
            if ctx.json {
                print_json(&p);
            } else {
                print_project(&p);
                print_line(
                    "Its rules are active now. Any change to the file makes it inactive until you trust it again.",
                );
            }
        }
        ProjectCmd::Untrust { dir } => {
            let p = client
                .call::<method::ProjectsUntrust>(ProjectDirParams { dir: absolute(dir)? })
                .await?;
            if ctx.json { print_json(&p) } else { print_project(&p) }
        }
    }
    Ok(ExitCode::SUCCESS)
}
