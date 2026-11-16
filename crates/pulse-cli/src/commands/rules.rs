//! `rules …` and `test-rule`.

use std::process::ExitCode;

use pulse_contracts::Outcome;
use pulse_contracts::config::{ConfigOrigin, RuleConfig};
use pulse_contracts::rpc::*;

use super::{Res, parse_id, time_arg};
use crate::cli::{OutcomeArg, RulesCmd, TestRuleArgs};
use crate::{Ctx, Failure, format, print_json, print_line};

fn flags(info: &RuleInfo) -> String {
    let c = &info.config;
    let mut f = Vec::new();
    if !c.enabled {
        f.push("disabled".to_owned());
    }
    if c.archived {
        f.push("archived".to_owned());
    }
    if c.supervised {
        f.push("supervised".to_owned());
    }
    if let Some(until) = info.state.snoozed_until {
        f.push(format!("snoozed until {until}"));
    }
    if let Some(until) = info.state.circuit_open_until {
        f.push(format!("breaker open until {until}"));
    }
    f.join(", ")
}

fn print_rule(info: &RuleInfo) {
    let on: Vec<&str> = info.config.on.0.iter().map(|g| g.as_str()).collect();
    print_line(&format!(
        "{:<24} on {:<24} -> {:<12} {}",
        info.config.name,
        on.join(","),
        info.config.run.agent,
        flags(info)
    ));
}

pub async fn rules(ctx: &Ctx, cmd: RulesCmd) -> Res {
    let client = ctx.client().await?;
    let info = match cmd {
        RulesCmd::List { all } => {
            let list =
                client.call::<method::RulesList>(RulesListParams { include_archived: all }).await?;
            if ctx.json {
                print_json(&list);
            } else if list.rules.is_empty() {
                print_line("No rules. `pulse init --list` shows templates.");
            } else {
                list.rules.iter().for_each(print_rule);
            }
            return Ok(ExitCode::SUCCESS);
        }
        RulesCmd::History { name, outcome, limit } => {
            let outcomes = outcome
                .into_iter()
                .map(|o| match o {
                    OutcomeArg::Fired => DeliveryKind::Fired,
                    OutcomeArg::AwaitingApproval => DeliveryKind::AwaitingApproval,
                    OutcomeArg::Debounced => DeliveryKind::Debounced,
                    OutcomeArg::Waiting => DeliveryKind::Waiting,
                    OutcomeArg::Blocked => DeliveryKind::Blocked,
                })
                .collect();
            let page = client
                .call::<method::RulesHistory>(RuleHistoryParams {
                    name,
                    outcomes,
                    cursor: None,
                    limit: Some(limit),
                })
                .await?;
            if ctx.json {
                print_json(&page);
            } else if page.deliveries.is_empty() {
                print_line("No decisions recorded for this rule yet.");
            } else {
                page.deliveries.iter().for_each(|d| print_line(&format::delivery_line(d)));
            }
            return Ok(ExitCode::SUCCESS);
        }
        RulesCmd::Show { name } => client.call::<method::RulesGet>(NameParams { name }).await?,
        RulesCmd::Add { file, project, replace } => {
            let rule: RuleConfig = super::read_definition(&file)?;
            let what = format!("a rule named {:?}", rule.name);
            let expected = super::add_expected(replace);
            let info = client
                .call::<method::RulesUpsert>(RuleUpsertParams { rule, project, expected })
                .await
                .map_err(|e| super::exists_hint(e, &what))?;
            super::warn_about(ctx, &client, &format!("rule {:?}", info.config.name)).await?;
            info
        }
        RulesCmd::Remove { name } => {
            let params = DeleteParams { name: name.clone(), expected: Expected::Any };
            client.call::<method::RulesDelete>(params).await?;
            if ctx.json {
                print_json(&serde_json::json!({ "removed": name }));
            } else {
                print_line(&format!("Removed rule {name}."));
            }
            return Ok(ExitCode::SUCCESS);
        }
        RulesCmd::Enable { name } => {
            client.call::<method::RulesSetEnabled>(SetEnabledParams { name, enabled: true }).await?
        }
        RulesCmd::Disable { name } => {
            client
                .call::<method::RulesSetEnabled>(SetEnabledParams { name, enabled: false })
                .await?
        }
        RulesCmd::Snooze { name, r#for } => {
            let until = time_arg(&r#for, false)?;
            client.call::<method::RulesSnooze>(SnoozeParams { name, until }).await?
        }
        RulesCmd::Unsnooze { name } => {
            client.call::<method::RulesUnsnooze>(NameParams { name }).await?
        }
        RulesCmd::Archive { name } => {
            client.call::<method::RulesArchive>(NameParams { name }).await?
        }
        RulesCmd::Reopen { name } => {
            client.call::<method::RulesReopen>(NameParams { name }).await?
        }
    };
    if ctx.json {
        print_json(&info);
        return Ok(ExitCode::SUCCESS);
    }
    let file = match &info.origin {
        ConfigOrigin::User { path } => path.display().to_string(),
        ConfigOrigin::Project { name, path, .. } => format!("{} (project {name})", path.display()),
    };
    print_line(&format!("# {} — in {file}", info.config.name));
    let state = flags(&info);
    if !state.is_empty() {
        print_line(&format!("# {state}"));
    }
    if let Some(at) = info.state.last_fired_at {
        print_line(&format!("# last fired {at}"));
    }
    print_line(&toml::to_string_pretty(&info.config).unwrap_or_default());
    Ok(ExitCode::SUCCESS)
}

pub async fn test_rule(ctx: &Ctx, args: TestRuleArgs) -> Res {
    let rule = match (&args.rule, &args.file) {
        (Some(name), None) => RuleRef::Saved { name: name.clone() },
        (None, Some(file)) => {
            let text = std::fs::read_to_string(file)?;
            let rule: RuleConfig = toml::from_str(&text)
                .map_err(|e| Failure::Message(format!("{}: {e}", file.display())))?;
            RuleRef::Draft { rule: Box::new(rule), project: args.project.clone() }
        }
        _ => return Err(Failure::Message("give a rule name or --file, not both".into())),
    };
    let client = ctx.client().await?;
    let res = client
        .call::<method::RulesTest>(RuleTestParams {
            rule,
            event: parse_id(&args.event)?,
            ignore_runtime_state: args.fresh,
        })
        .await?;
    if ctx.json {
        print_json(&res);
        return Ok(ExitCode::SUCCESS);
    }
    let Some(decision) = &res.decision else {
        print_line(
            "No match: the event's type or `where` does not match this rule (or it is archived).",
        );
        return Ok(ExitCode::SUCCESS);
    };
    print_line(&format!("decision: {}", format::decision(decision)));
    let mut shown = &decision.outcome;
    if let Some(after) = &res.after_debounce {
        print_line(&format!("after the debounce: {}", format::outcome(&after.outcome)));
        shown = &after.outcome;
    }
    match format::prompt_of(shown) {
        Some(prompt) => {
            print_line("prompt:");
            print_line(prompt);
        }
        None if matches!(shown, Outcome::Fire { .. } | Outcome::AwaitApproval { .. }) => {
            print_line("prompt: none (a command agent without a prompt)");
        }
        None => {}
    }
    Ok(ExitCode::SUCCESS)
}
