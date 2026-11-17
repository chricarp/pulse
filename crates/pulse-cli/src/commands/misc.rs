//! `agents`, `sources`, `project`.

use std::path::PathBuf;
use std::process::ExitCode;

use pulse_contracts::config::{AgentConfig, AgentKind, ConfigOrigin, SourceConfig};
use pulse_contracts::rpc::*;
use pulse_contracts::status::{Project, TrustState};

use super::Res;
use crate::cli::{AgentsArgs, AgentsCmd, ProjectCmd, SourceKindArg, SourcesArgs, SourcesCmd};
use crate::output::{Out, Text, note};
use crate::{Ctx, Failure};

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
            let Some(out) = ctx.answer(&list) else { return Ok(ExitCode::SUCCESS) };
            if list.agents.is_empty() {
                out.line("No agents.");
            }
            for a in &list.agents {
                let scopes: Vec<String> = a.config.allow.iter().map(ToString::to_string).collect();
                out.line(&format!(
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
            if let Some(out) = ctx.answer(&a) {
                out.line(&format!(
                    "# {} — running {}, queued {}{}",
                    a.config.name,
                    a.running,
                    a.queued,
                    origin(&a.origin)
                ));
                out.line(&toml::to_string_pretty(&a.config).unwrap_or_default());
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
            if let Some(out) = ctx.answer(&a) {
                out.line(&format!("Saved agent {}{}.", a.config.name, origin(&a.origin)));
            }
            super::warn_about(ctx, &client, &format!("agent {:?}", a.config.name)).await?;
        }
        AgentsCmd::Remove { name } => {
            let params = DeleteParams { name: name.clone(), expected: Expected::Any };
            client.call::<method::AgentsDelete>(params).await?;
            if let Some(out) = ctx.answer(&serde_json::json!({ "removed": name })) {
                out.line(&format!("Removed agent {name}."));
            }
        }
        AgentsCmd::McpCredentials { name } => {
            let c = client.call::<method::AgentsMcpCredentials>(NameParams { name }).await?;
            if let Some(out) = ctx.answer(&c) {
                out.line(&super::mcp::credentials_text(ctx, &c));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `sources enable` / `disable`: one field of the source as it is in its file.
async fn set_source_enabled(
    ctx: &Ctx,
    client: &pulse_daemon::client::Client,
    name: String,
    enabled: bool,
) -> Res {
    let s = client.call::<method::SourcesSetEnabled>(SetEnabledParams { name, enabled }).await?;
    if let Some(out) = ctx.answer(&s) {
        let done = if enabled { "Enabled" } else { "Disabled" };
        out.line(&format!("{done} source {}{}.", s.config.name, origin(&s.origin)));
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn sources(ctx: &Ctx, args: SourcesArgs) -> Res {
    let client = ctx.client().await?;
    match args.cmd.unwrap_or(SourcesCmd::List) {
        SourcesCmd::List => {}
        SourcesCmd::Enable { name } => return set_source_enabled(ctx, &client, name, true).await,
        SourcesCmd::Disable { name } => return set_source_enabled(ctx, &client, name, false).await,
        SourcesCmd::Restart { name } => {
            let s = client.call::<method::SourcesRestart>(NameParams { name }).await?;
            if let Some(out) = ctx.answer(&s) {
                out.line(&format!("Restarted source {}{}.", s.config.name, origin(&s.origin)));
            }
            return Ok(ExitCode::SUCCESS);
        }
        SourcesCmd::Remove { name } => {
            let params = DeleteParams { name: name.clone(), expected: Expected::Any };
            client.call::<method::SourcesDelete>(params).await?;
            if let Some(out) = ctx.answer(&serde_json::json!({ "removed": name })) {
                out.line(&format!("Removed source {name}."));
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
            if let Some(out) = ctx.answer(&s) {
                out.line(&format!(
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
    let Some(out) = ctx.answer(&list) else { return Ok(ExitCode::SUCCESS) };
    if list.sources.is_empty() {
        out.line("No sources.");
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
        out.line(&line);
        if let Some(e) = h.last_error.as_ref().filter(|e| h.message.as_ref() != Some(&e.message)) {
            out.line(&format!("    last problem {}: {}", e.at, e.message));
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
            return Err(Failure::invalid(format!("a schedule takes --cron or --every, not {t:?}")));
        }
        (SourceKindArg::Schedule, None) => {}
        (_, None) => {
            return Err(Failure::invalid(format!("a {kind_name} source needs a path (or URL)")));
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
        .map_err(|e: toml::de::Error| Failure::invalid(e.message().to_owned()))
}

fn absolute(dir: PathBuf) -> Result<String, Failure> {
    let abs = if dir.is_absolute() { dir } else { std::env::current_dir()?.join(dir) };
    Ok(abs.display().to_string())
}

fn print_project(out: &Text, p: &Project) {
    let state = match p.state {
        TrustState::Trusted => "trusted",
        TrustState::Changed => "changed since trusted: inactive until `pulse project trust` again",
        TrustState::Missing => "pulse.toml is missing",
        TrustState::Untrusted => "not trusted",
    };
    out.line(&format!("{:<16} {}  {state}", p.name, p.dir));
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
/// from the repository; `Text::line` escapes terminal control characters.
fn print_review(out: &Text, r: &ProjectReview) {
    let state = match r.state {
        TrustState::Untrusted => "not trusted",
        TrustState::Trusted => "trusted, unchanged",
        TrustState::Changed => "changed since it was trusted",
        TrustState::Missing => "missing",
    };
    out.line(&format!("# {} ({state})", r.file));
    out.line(&format!("# definitions are named {}/…", r.name));
    for e in &r.errors {
        out.line(&format!("error: {e}"));
    }
    if let Some(changes) = &r.changes {
        out.line("");
        if changes.is_empty() {
            out.line(
                "Changes since it was trusted: none in its definitions (only formatting or comments).",
            );
        } else {
            out.line("Changes since it was trusted:");
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
            out.line(&format!("  {kind} {}: {what}", c.name));
        }
    }
    for a in &r.agents {
        out.line("");
        out.line("# agent");
        out.line(toml::to_string_pretty(a).unwrap_or_default().trim_end());
        // Where its runs start, resolved: a relative `cwd` is the project's directory.
        match r.cwds.get(&a.name) {
            Some(dir) => out.line(&format!("# runs in {dir}")),
            None if a.cwd.is_none() && a.kind() != AgentKind::Mcp => {
                out.line("# runs in its own directory under the Pulse home")
            }
            None => {}
        }
    }
    let defs = [
        ("rule", r.rules.iter().map(toml::to_string_pretty).collect::<Vec<_>>()),
        ("source", r.sources.iter().map(toml::to_string_pretty).collect()),
    ];
    for (kind, items) in defs {
        for item in items {
            out.line("");
            out.line(&format!("# {kind}"));
            out.line(item.unwrap_or_default().trim_end());
        }
    }
    out.line("");
    out.line(&format!("# content hash {}", r.hash));
}

pub async fn project(ctx: &Ctx, cmd: ProjectCmd) -> Res {
    let client = ctx.client().await?;
    match cmd {
        ProjectCmd::List => {
            let list = client.call::<method::ProjectsList>(Empty {}).await?;
            if let Some(out) = ctx.answer(&list) {
                if list.projects.is_empty() {
                    out.line("No trusted projects.");
                }
                list.projects.iter().for_each(|p| print_project(&out, p));
            }
        }
        ProjectCmd::Trust { dir, name, hash: None } => {
            // Looking and trusting are two calls; binding the second to the hash the first
            // printed is what makes the user trust what they read, interactively or not.
            let dir = absolute(dir)?;
            let review = client
                .call::<method::ProjectsReview>(ProjectReviewParams { dir: dir.clone(), name })
                .await?;
            let advice = if review.errors.is_empty() {
                format!(
                    "Nothing was trusted. To trust exactly this content:\n  pulse project trust {} --name {} --hash {}",
                    shell_word(&review.dir),
                    review.name,
                    review.hash
                )
            } else {
                "Nothing was trusted: the file has errors.".to_owned()
            };
            // A review trusts nothing, so it does not succeed: under --json it is the error's data.
            let Out::Text(out) = ctx.out() else {
                let review = serde_json::to_value(&review).unwrap_or_default();
                return Err(Failure::new("not_trusted", advice).with_data(review));
            };
            print_review(&out, &review);
            note(&advice);
            return Ok(ExitCode::FAILURE);
        }
        ProjectCmd::Trust { dir, name, hash: Some(hash) } => {
            let params = ProjectTrustParams { dir: absolute(dir)?, name, hash };
            let p = client.call::<method::ProjectsTrust>(params).await.map_err(|e| match e {
                pulse_daemon::client::ClientError::Rpc(err)
                    if err.kind() == Some(ErrorCode::Stale) =>
                {
                    Failure::from(err).with_message(
                        "the file changed since that hash was printed; nothing was trusted. Run `pulse project trust` without --hash to review it again.",
                    )
                }
                other => other.into(),
            })?;
            if let Some(out) = ctx.answer(&p) {
                print_project(&out, &p);
                out.line(
                    "Its rules are active now. Any change to the file makes it inactive until you trust it again.",
                );
            }
        }
        ProjectCmd::Untrust { dir } => {
            let p = client
                .call::<method::ProjectsUntrust>(ProjectDirParams { dir: absolute(dir)? })
                .await?;
            if let Some(out) = ctx.answer(&p) {
                print_project(&out, &p);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
