//! `tail`, `events`, `replay`, `ingest`, `run`.

use std::io::BufRead;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use pulse_contracts::rpc::*;
use pulse_contracts::{EventId, RunId};
use pulse_daemon::client::Client;

use super::{Res, event_id, parse_id, time_arg};
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
    let runs = if args.no_runs {
        None
    } else {
        Some(client.call::<method::RunsSubscribe>(RunsSubscribeParams::default()).await?)
    };
    // Read after subscribing: a run of any later event waits for it (see `tail::Order`).
    let newest = EventQuery { limit: Some(1), ..EventQuery::default() };
    let newest =
        client.call::<method::EventsQuery>(newest).await?.page.events.first().map(|e| e.id);
    let mut order = crate::tail::Order::new(newest);
    // For people on stderr in both modes; it also tells a script that tail is subscribed.
    note("Following events and runs; Ctrl-C stops.");
    let out = ctx.out();
    let show_runs = |runs_changed: RunsChanged| {
        if runs_changed.runs.is_empty() {
            return;
        }
        match &out {
            Out::Json(json) => json.line(&Notification::RunsChanged(runs_changed)),
            Out::Text(text) => {
                for r in &runs_changed.runs {
                    text.line(&format!("run {}", format::run_line(r)));
                }
            }
        }
    };
    while let Some(n) = notes.recv().await {
        match n {
            Notification::EventsBatch(b) => {
                let batch = &b.batch;
                let ready = order.events(batch.events.iter().map(|e| e.id), batch.dropped > 0);
                match &out {
                    Out::Json(json) => json.line(&Notification::EventsBatch(b.clone())),
                    Out::Text(text) => {
                        for e in &batch.events {
                            text.line(&format::event_line(e));
                        }
                        if batch.dropped > 0 {
                            text.line(&format!(
                                "… {} more not shown live (the log has them: pulse events)",
                                batch.dropped
                            ));
                        }
                    }
                }
                if let Some(subscribed) = &runs {
                    show_runs(RunsChanged { subscription: subscribed.subscription, runs: ready });
                }
            }
            Notification::RunsChanged(c) => {
                let ready = order.runs(c.runs);
                show_runs(RunsChanged { subscription: c.subscription, runs: ready });
            }
            other => {
                if let Out::Json(json) = &out {
                    json.line(&other);
                }
            }
        }
    }
    Err(Failure::connection("the daemon closed the connection"))
}

pub async fn events(ctx: &Ctx, args: EventsArgs) -> Res {
    let client = ctx.client().await?;
    if let Some(EventsCmd::Show { id }) = args.show {
        let detail = client
            .call::<method::EventsGet>(EventIdParams { id: event_id(&client, &id).await? })
            .await?;
        let Some(out) = ctx.answer(&detail) else { return Ok(ExitCode::SUCCESS) };
        out.line(&format::event_line(&detail.event));
        out.line(&serde_json::to_string_pretty(&detail.event.data).unwrap_or_default());
        if detail.deliveries.is_empty() {
            out.line("No rule matched this event.");
        }
        for d in &detail.deliveries {
            out.line(&format::delivery_line(d, detail.event.causation));
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
        .call::<method::EventsReplay>(ReplayParams {
            id: event_id(&client, &args.event).await?,
            rules,
        })
        .await?;
    let Some(out) = ctx.answer(&res) else { return Ok(ExitCode::SUCCESS) };
    out.line(&format!("Replayed as {}", res.event.id));
    if res.deliveries.is_empty() {
        out.line("No rule matched.");
    }
    for d in &res.deliveries {
        out.line(&format::delivery_line(d, res.event.causation));
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

/// One stdin line, parsed exactly as `POST /ingest` parses a line of JSON lines (one
/// implementation, `pulse_sources::ingest::parse`): the same fields, CloudEvents, limits and
/// refusals. Returns what is wrong with it, without the line number the parser counts.
fn parse_line(line: &str) -> Result<Vec<IngestEvent>, String> {
    use pulse_sources::ingest::{IngestError, IngestLimits, parse};
    parse(Some("application/x-ndjson"), line.as_bytes(), &IngestLimits::default()).map_err(|e| {
        match e {
            IngestError::Parse { message, .. } => message,
            IngestError::Invalid(items) => items
                .iter()
                .map(|i| match &i.field {
                    Some(field) => format!("{field}: {}", i.message),
                    None => i.message.clone(),
                })
                .collect::<Vec<_>>()
                .join("; "),
            other => other.to_string(),
        }
    })
}

pub async fn ingest(ctx: &Ctx, args: IngestArgs) -> Res {
    const BATCH: usize = 100;
    let client = ctx.client().await?;
    let cause = own_run();
    let mut lines = stdin_lines();
    let (mut accepted, mut duplicates, mut number) = (0usize, 0u32, 0usize);
    let mut batch = Vec::new();
    let mut bad_line = None;
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
            match parse_line(&line) {
                Ok(events) => batch.extend(events.into_iter().map(|mut event| {
                    event.causation_run = event.causation_run.or(cause);
                    event
                })),
                Err(why) => {
                    // The lines before it are sent; this one and the rest are not.
                    bad_line = Some(why);
                    break;
                }
            }
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
    if let Some(why) = bad_line {
        let data =
            serde_json::json!({ "line": number, "accepted": accepted, "duplicates": duplicates });
        let sent = if accepted == 0 && duplicates == 0 {
            "nothing was ingested".to_owned()
        } else {
            format!("the lines before it were ({accepted} ingested, {duplicates} duplicates)")
        };
        return Err(Failure::invalid(format!("stdin line {number}: {why}; {sent}")).with_data(data));
    }
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
