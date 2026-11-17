//! `rules …` and `test-rule`.

use std::process::ExitCode;

use pulse_contracts::config::{ConfigOrigin, RuleConfig};
use pulse_contracts::rpc::*;
use pulse_contracts::{BlockReason, Outcome};

use super::{Res, time_arg};
use crate::cli::{OutcomeArg, RulesCmd, TestRuleArgs};
use crate::output::Text;
use crate::{Ctx, Failure, format};

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

fn print_rule(out: &Text, info: &RuleInfo) {
    let on: Vec<&str> = info.config.on.0.iter().map(|g| g.as_str()).collect();
    out.line(&format!(
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
            if let Some(out) = ctx.answer(&list) {
                if list.rules.is_empty() {
                    out.line("No rules. `pulse init --list` shows templates.");
                }
                list.rules.iter().for_each(|r| print_rule(&out, r));
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
            if let Some(out) = ctx.answer(&page) {
                if page.deliveries.is_empty() {
                    out.line("No decisions recorded for this rule yet.");
                }
                for d in &page.deliveries {
                    out.line(&format::delivery_line(d, page.causes.get(&d.event).copied(), None));
                }
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
            if let Some(out) = ctx.answer(&serde_json::json!({ "removed": name })) {
                out.line(&format!("Removed rule {name}."));
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
    let Some(out) = ctx.answer(&info) else { return Ok(ExitCode::SUCCESS) };
    let file = match &info.origin {
        ConfigOrigin::User { path } => path.display().to_string(),
        ConfigOrigin::Project { name, path, .. } => format!("{} (project {name})", path.display()),
    };
    out.line(&format!("# {} — in {file}", info.config.name));
    let state = flags(&info);
    if !state.is_empty() {
        out.line(&format!("# {state}"));
    }
    if let Some(at) = info.state.last_fired_at {
        out.line(&format!("# last fired {at}"));
    }
    out.line(&toml::to_string_pretty(&info.config).unwrap_or_default());
    Ok(ExitCode::SUCCESS)
}

pub async fn test_rule(ctx: &Ctx, args: TestRuleArgs) -> Res {
    let rule = match (&args.rule, &args.file) {
        (Some(name), None) => RuleRef::Saved { name: name.clone() },
        (None, Some(file)) => {
            let text = std::fs::read_to_string(file)?;
            let rule: RuleConfig = toml::from_str(&text)
                .map_err(|e| Failure::invalid(format!("{}: {e}", file.display())))?;
            RuleRef::Draft { rule: Box::new(rule), project: args.project.clone() }
        }
        _ => return Err(Failure::usage("give a rule name or --file, not both")),
    };
    let client = ctx.client().await?;
    let res = client
        .call::<method::RulesTest>(RuleTestParams {
            rule,
            event: super::event_id(&client, &args.event).await?,
            ignore_runtime_state: args.fresh,
        })
        .await?;
    let Some(out) = ctx.answer(&res) else { return Ok(ExitCode::SUCCESS) };
    let Some(decision) = &res.decision else {
        out.line(
            "No match: the event's type or `where` does not match this rule (or it is archived).",
        );
        return Ok(ExitCode::SUCCESS);
    };
    out.line(&format!("decision: {}", format::decision(decision)));
    // Loop protection comes from the event's recorded cause, which no flag of a dry run
    // changes; a replay is a copy without it.
    if let Outcome::Blocked {
        reason: BlockReason::LoopSelfTrigger { .. } | BlockReason::LoopChainDepth { .. },
    } = &decision.outcome
    {
        let what = if args.fresh {
            "--fresh ignores the rule's history (snooze, cooldown, rate limit), not the event's cause"
        } else {
            "this comes from the event's cause"
        };
        let rule = args.rule.as_deref().map(|r| format!(" --rule {r}")).unwrap_or_default();
        out.line(&format!(
            "note: {what}: the event is recorded as caused by a run, so loop protection applies to it. If you made this change yourself, `pulse replay {}{rule}` routes a copy without the cause.",
            args.event
        ));
    }
    let mut shown = &decision.outcome;
    if let Some(after) = &res.after_debounce {
        out.line(&format!("after the debounce: {}", format::outcome(&after.outcome)));
        shown = &after.outcome;
    }
    match format::prompt_of(shown) {
        Some(prompt) => {
            out.line("prompt:");
            out.line(prompt);
        }
        None if matches!(shown, Outcome::Fire { .. } | Outcome::AwaitApproval { .. }) => {
            out.line("prompt: none (a command agent without a prompt)");
        }
        None => {}
    }
    Ok(ExitCode::SUCCESS)
}
