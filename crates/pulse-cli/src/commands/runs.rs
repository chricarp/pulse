//! `runs …`.

use std::process::ExitCode;

use pulse_contracts::rpc::*;
use pulse_contracts::run::RunOutputChunk;
use pulse_contracts::{RunId, RunStatus};

use super::{Res, parse_id};
use crate::cli::RunsCmd;
use crate::{Ctx, Failure, eprint_line, format, print_json, print_line};

fn print_chunks(ctx: &Ctx, chunks: &[RunOutputChunk]) {
    for c in chunks {
        if ctx.json {
            if let Ok(line) = serde_json::to_string(c) {
                print_line(&line);
            }
            continue;
        }
        let (text, err) = format::chunk(c);
        if err { eprint_line(&text) } else { print_line(&text) }
    }
}

pub async fn runs(ctx: &Ctx, cmd: RunsCmd) -> Res {
    let client = ctx.client().await?;
    let summary = match cmd {
        RunsCmd::List { rule, agent, status, limit } => {
            let status = status
                .iter()
                .map(|s| {
                    RunStatus::parse(s)
                        .ok_or_else(|| Failure::Message(format!("unknown run status {s:?}")))
                })
                .collect::<Result<_, _>>()?;
            let page = client
                .call::<method::RunsList>(RunsListParams {
                    filter: RunFilter { rule, agent, status, event: None },
                    cursor: None,
                    limit: Some(limit),
                })
                .await?;
            if ctx.json {
                print_json(&page);
            } else if page.runs.is_empty() {
                print_line("No runs.");
            } else {
                page.runs.iter().for_each(|r| print_line(&format::run_line(r)));
            }
            return Ok(ExitCode::SUCCESS);
        }
        RunsCmd::Show { id } => {
            let run = client.call::<method::RunsGet>(RunIdParams { id: parse_id(&id)? }).await?;
            if ctx.json {
                print_json(&run);
                return Ok(ExitCode::SUCCESS);
            }
            print_line(&format::run_line(&run.summary()));
            print_line(&format!("event    {}", run.event));
            print_line(&format!("created  {}", run.created_at));
            if let Some(t) = run.started_at {
                print_line(&format!("started  {t}"));
            }
            if let Some(t) = run.ended_at {
                print_line(&format!("ended    {t}"));
            }
            if let Some(inv) = &run.invocation {
                print_line(&format!("ran      {} {:?} in {}", inv.program, inv.args, inv.cwd));
            }
            if let Some(e) = &run.error {
                print_line(&format!("error    {e}"));
            }
            print_line(&format!(
                "output   {} bytes ({} dropped)",
                run.output.bytes, run.output.dropped_bytes
            ));
            if let Some(p) = &run.prompt {
                print_line("prompt:");
                print_line(p);
            }
            return Ok(ExitCode::SUCCESS);
        }
        RunsCmd::Logs { id, follow } => {
            let id: RunId = parse_id(&id)?;
            if follow {
                let mut notes = client
                    .notifications()
                    .ok_or_else(|| Failure::Message("notifications taken".into()))?;
                client
                    .call::<method::RunsSubscribeOutput>(RunOutputSubscribeParams {
                        id,
                        after: None,
                    })
                    .await?;
                while let Some(n) = notes.recv().await {
                    if let Notification::RunsOutput(o) = n {
                        print_chunks(ctx, &o.chunks);
                        if o.done {
                            let run = client.call::<method::RunsGet>(RunIdParams { id }).await?;
                            if !ctx.json {
                                match &run.error {
                                    Some(e) => {
                                        eprint_line(&format!("[run {}: {e}]", run.status.as_str()))
                                    }
                                    None => eprint_line(&format!("[run {}]", run.status.as_str())),
                                }
                            }
                            return Ok(ExitCode::SUCCESS);
                        }
                    }
                }
                return Err(Failure::Message("the daemon closed the connection".into()));
            }
            let mut after = None;
            loop {
                let page = client
                    .call::<method::RunsOutputGet>(RunOutputParams { id, after, limit: Some(500) })
                    .await?;
                print_chunks(ctx, &page.chunks);
                if page.chunks.is_empty() || page.next_after == after {
                    break;
                }
                after = page.next_after;
            }
            // A run that never started has no output; say why instead of printing nothing.
            if !ctx.json && after.is_none() {
                let run = client.call::<method::RunsGet>(RunIdParams { id }).await?;
                if let Some(e) = &run.error {
                    eprint_line(&format!("[run {}: {e}]", run.status.as_str()));
                }
            }
            return Ok(ExitCode::SUCCESS);
        }
        RunsCmd::Approve { id } => {
            client.call::<method::RunsApprove>(RunIdParams { id: parse_id(&id)? }).await?
        }
        RunsCmd::Reject { id, reason } => {
            client.call::<method::RunsReject>(RejectParams { id: parse_id(&id)?, reason }).await?
        }
        RunsCmd::Cancel { id } => {
            client.call::<method::RunsCancel>(RunIdParams { id: parse_id(&id)? }).await?
        }
        RunsCmd::Rerun { id } => {
            client.call::<method::RunsRerun>(RunIdParams { id: parse_id(&id)? }).await?
        }
    };
    if ctx.json {
        print_json(&summary);
    } else {
        print_line(&format::run_line(&summary));
    }
    Ok(ExitCode::SUCCESS)
}
