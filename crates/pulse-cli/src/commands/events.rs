//! `tail`, `events`, `replay`, `ingest`, `run`.

use std::io::BufRead;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use pulse_contracts::rpc::*;
use pulse_contracts::{EventId, RunId};
use pulse_daemon::client::Client;

use super::{Res, parse_id, time_arg};
use crate::cli::{EventsArgs, EventsCmd, FilterArgs, IngestArgs, ReplayArgs, RunArgs, TailArgs};
use crate::output::{Out, note};
use crate::{Ctx, Failure, format};

fn filter(f: FilterArgs) -> Result<EventFilter, Failure> {
    let types = f
        .types
        .iter()
        .map(|t| t.parse().map_err(|e| Failure::invalid(format!("--type {t:?}: {e}"))))
        .collect::<Result<_, _>>()?;
    let path = match f.path {
        Some(p) => {
            let abs = if p.is_absolute() { p } else { std::env::current_dir()?.join(p) };
            Some(abs.display().to_string())
        }
        None => None,
    };
    Ok(EventFilter {
        types,
        sources: f.sources,
        subject: f.subject,
        path,
        text: f.text,
        ..EventFilter::default()
    })
}

/// The run this process belongs to, if it was started by a Pulse run.
fn own_run() -> Option<RunId> {
    std::env::var("PULSE_RUN_ID").ok()?.parse().ok()
}

pub async fn tail(ctx: &Ctx, args: TailArgs) -> Res {
    let client = ctx.client().await?;
    let mut notes =
        client.notifications().ok_or_else(|| Failure::internal("notifications taken"))?;
    client
        .call::<method::EventsSubscribe>(EventsSubscribeParams { filter: filter(args.filter)? })
        .await?;
    if !args.no_runs {
        client.call::<method::RunsSubscribe>(RunsSubscribeParams::default()).await?;
    }
    // For people on stderr in both modes; it also tells a script that tail is subscribed.
    note("Following events and runs; Ctrl-C stops.");
    let out = ctx.out();
    while let Some(n) = notes.recv().await {
        let text = match &out {
            Out::Json(json) => {
                json.line(&n);
                continue;
            }
            Out::Text(text) => text,
        };
        match n {
            Notification::EventsBatch(b) => {
                let b = b.batch;
                for e in &b.events {
                    text.line(&format::event_line(e));
                }
                if b.dropped > 0 {
                    text.line(&format!(
                        "… {} more not shown live (the log has them: pulse events)",
                        b.dropped
                    ));
                }
            }
            Notification::RunsChanged(c) => {
                for r in &c.runs {
                    text.line(&format!("run {}", format::run_line(r)));
                }
            }
            _ => {}
        }
    }
    Err(Failure::connection("the daemon closed the connection"))
}

pub async fn events(ctx: &Ctx, args: EventsArgs) -> Res {
    let client = ctx.client().await?;
    if let Some(EventsCmd::Show { id }) = args.show {
        let detail = client.call::<method::EventsGet>(EventIdParams { id: parse_id(&id)? }).await?;
        let Some(out) = ctx.answer(&detail) else { return Ok(ExitCode::SUCCESS) };
        out.line(&format::event_line(&detail.event));
        out.line(&serde_json::to_string_pretty(&detail.event.data).unwrap_or_default());
        if detail.deliveries.is_empty() {
            out.line("No rule matched this event.");
        }
        for d in &detail.deliveries {
            out.line(&format::delivery_line(d));
        }
        for r in &detail.runs {
            out.line(&format!("run {}", format::run_line(r)));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut f = filter(args.filter)?;
    f.since = args.since.as_deref().map(|s| time_arg(s, true)).transpose()?;
    let query = EventQuery {
        filter: f,
        cursor: args.cursor.as_deref().map(parse_id::<EventId>).transpose()?,
        limit: Some(args.limit),
        order: if args.oldest_first { Order::OldestFirst } else { Order::NewestFirst },
    };
    let page = client.call::<method::EventsQuery>(query).await?;
    let Some(out) = ctx.answer(&page) else { return Ok(ExitCode::SUCCESS) };
    for e in &page.page.events {
        out.line(&format::event_line(e));
    }
    if let Some(next) = page.page.next_cursor {
        note(&format!("More: pulse events --cursor {next}"));
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn replay(ctx: &Ctx, args: ReplayArgs) -> Res {
    let client = ctx.client().await?;
    let rules = (!args.rule.is_empty()).then_some(args.rule);
    let res = client
        .call::<method::EventsReplay>(ReplayParams { id: parse_id(&args.event)?, rules })
        .await?;
    let Some(out) = ctx.answer(&res) else { return Ok(ExitCode::SUCCESS) };
    out.line(&format!("Replayed as {}", res.event.id));
    if res.deliveries.is_empty() {
        out.line("No rule matched.");
    }
    for d in &res.deliveries {
        out.line(&format::delivery_line(d));
    }
    Ok(ExitCode::SUCCESS)
}

/// Lines from stdin, read on a thread so a slow producer never blocks the connection.
fn stdin_lines() -> tokio::sync::mpsc::Receiver<std::io::Result<String>> {
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if tx.blocking_send(line).is_err() {
                break;
            }
        }
    });
    rx
}

pub async fn ingest(ctx: &Ctx, args: IngestArgs) -> Res {
    const BATCH: usize = 100;
    let client = ctx.client().await?;
    let cause = own_run();
    let mut lines = stdin_lines();
    let (mut accepted, mut duplicates, mut number) = (0usize, 0u32, 0usize);
    let mut batch = Vec::new();
    loop {
        // Send what is buffered as soon as stdin has nothing more ready.
        let next = match lines.try_recv() {
            Ok(l) => Some(l),
            Err(_) if !batch.is_empty() => None,
            Err(_) => match lines.recv().await {
                Some(l) => Some(l),
                None => break,
            },
        };
        if let Some(line) = next {
            number += 1;
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let mut event: IngestEvent = serde_json::from_str(&line)
                .map_err(|e| Failure::invalid(format!("stdin line {number}: {e}")))?;
            if event.causation_run.is_none() {
                event.causation_run = cause;
            }
            batch.push(event);
            if batch.len() < BATCH {
                continue;
            }
        }
        let events = std::mem::take(&mut batch);
        let res = client
            .call::<method::EventsIngest>(IngestParams { source: args.source.clone(), events })
            .await?;
        accepted += res.accepted.len();
        duplicates += res.duplicates;
    }
    if !batch.is_empty() {
        let res = client
            .call::<method::EventsIngest>(IngestParams {
                source: args.source.clone(),
                events: batch,
            })
            .await?;
        accepted += res.accepted.len();
        duplicates += res.duplicates;
    }
    let res = serde_json::json!({ "accepted": accepted, "duplicates": duplicates });
    if ctx.answer(&res).is_some() {
        note(&format!("Ingested {accepted} events ({duplicates} duplicates dropped)."));
    }
    Ok(ExitCode::SUCCESS)
}

/// Reports how the command exited; silent when the daemon is unreachable. Never takes long:
/// the command has already finished and the caller is waiting for its exit code.
async fn report(home: &Path, data: pulse_contracts::catalog::ProcessExitedData) {
    let attempt = async {
        let client = Client::connect(home).await.ok()?;
        client
            .call::<method::EventsReport>(ReportParams {
                event: ReportedEvent::ProcessExited(data),
                causation_run: own_run(),
            })
            .await
            .ok()
    };
    let _ = tokio::time::timeout(Duration::from_secs(3), attempt).await;
}

pub async fn run(ctx: &Ctx, args: RunArgs) -> Res {
    let cwd = std::env::current_dir()?;
    let data = match pulse_sources::process::run_wrapped(&args.command, &cwd, args.tail_bytes).await
    {
        Ok(d) => d,
        Err(e) => {
            note(&format!("pulse run: {e}"));
            // As a shell does for a command it cannot run.
            return Ok(ExitCode::from(127));
        }
    };
    let code = match (data.exit_code, data.signal) {
        (Some(c), _) => u8::try_from(c.rem_euclid(256)).unwrap_or(1),
        (None, Some(sig)) => u8::try_from((128 + sig).clamp(0, 255)).unwrap_or(1),
        (None, None) => 1,
    };
    report(&ctx.home, data).await;
    Ok(ExitCode::from(code))
}
