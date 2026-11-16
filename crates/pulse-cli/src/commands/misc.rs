//! `agents`, `sources`, `project`.

use std::path::PathBuf;
use std::process::ExitCode;

use pulse_contracts::config::ConfigOrigin;
use pulse_contracts::rpc::*;
use pulse_contracts::status::{Project, TrustState};

use super::Res;
use crate::cli::{AgentsArgs, AgentsCmd, ProjectCmd};
use crate::{Ctx, Failure, print_json, print_line};

fn origin(o: &ConfigOrigin) -> String {
    match o {
        ConfigOrigin::User { .. } => String::new(),
        ConfigOrigin::Project { name, .. } => format!(" (project {name})"),
    }
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
                    format!("{:?}", a.config.kind()).to_lowercase(),
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
        AgentsCmd::Mcp { name } => {
            let c = client.call::<method::AgentsMcpCredentials>(NameParams { name }).await?;
            if ctx.json {
                print_json(&c);
            } else {
                print_line(&format!("url    {}", c.url));
                print_line(&format!("token  {}", c.token));
                print_line(
                    "The token works only on /mcp, only as this agent: it cannot approve runs or edit rules.",
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn sources(ctx: &Ctx) -> Res {
    let client = ctx.client().await?;
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
    }
    Ok(ExitCode::SUCCESS)
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
        ProjectCmd::Trust { dir, name } => {
            let p = client
                .call::<method::ProjectsTrust>(ProjectTrustParams { dir: absolute(dir)?, name })
                .await?;
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
