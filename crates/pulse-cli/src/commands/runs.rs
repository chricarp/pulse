//! `runs …`.

use std::process::ExitCode;

use pulse_contracts::rpc::*;
use pulse_contracts::run::RunOutputChunk;
use pulse_contracts::{RunId, RunStatus};

use super::{Res, parse_id};
use crate::cli::RunsCmd;
use crate::output::{Out, note};
use crate::{Ctx, Failure, format};

fn print_chunks(ctx: &Ctx, chunks: &[RunOutputChunk]) {
    let out = ctx.out();
    for c in chunks {
        match &out {
            Out::Json(json) => json.line(c),
            Out::Text(text) => {
                let (line, err) = format::chunk(c);
                if err { note(&line) } else { text.line(&line) }
            }
        }
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
            if let Some(out) = ctx.answer(&page) {
                if page.runs.is_empty() {
                    out.line("No runs.");
                }
                page.runs.iter().for_each(|r| out.line(&format::run_line(r)));
            }
            return Ok(ExitCode::SUCCESS);
        }
        RunsCmd::Show { id } => {
            let run = client.call::<method::RunsGet>(RunIdParams { id: parse_id(&id)? }).await?;
            let Some(out) = ctx.answer(&run) else { return Ok(ExitCode::SUCCESS) };
            out.line(&format::run_line(&run.summary()));
            out.line(&format!("event    {}", run.event));
            out.line(&format!("created  {}", run.created_at));
            if let Some(t) = run.started_at {
                out.line(&format!("started  {t}"));
            }
            if let Some(t) = run.ended_at {
                out.line(&format!("ended    {t}"));
            }
            // Rendered when the run was created: for a run awaiting approval, what approving
            // it will execute (and it starts only if that is still so).
            let verb = if run.started_at.is_some() { "ran     " } else { "will run" };
            match &run.invocation {
                Some(inv) => {
                    out.line(&format!("{verb} {} {:?} in {}", inv.program, inv.args, inv.cwd))
                }
                None if run.started_at.is_none() && !run.status.is_terminal() => {
                    out.line("will run (could not be prepared; the run will fail to start)")
                }
                None => {}
            }
            if let Some(e) = &run.error {
                out.line(&format!("error    {e}"));
            }
            out.line(&format!(
                "output   {} bytes ({} dropped)",
                run.output.bytes, run.output.dropped_bytes
            ));
            if let Some(p) = &run.prompt {
                out.line("prompt:");
                out.line(p);
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
                                    Some(e) => note(&format!("[run {}: {e}]", run.status.as_str())),
                                    None => note(&format!("[run {}]", run.status.as_str())),
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
                    note(&format!("[run {}: {e}]", run.status.as_str()));
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
    if let Some(out) = ctx.answer(&summary) {
        out.line(&format::run_line(&summary));
    }
    Ok(ExitCode::SUCCESS)
}
