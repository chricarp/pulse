//! Runs: creation, state transitions, output with a head-and-tail cap, listing.

use std::collections::BTreeMap;

use pulse_contracts::rpc::{RunFilter, RunOutputPage, RunPage, page_limit};
use pulse_contracts::run::{
    InvocationSummary, OutputKind, OutputStats, RunOutputChunk, RunSnapshot, RunSummary,
};
use pulse_contracts::status::RunCounts;
use pulse_contracts::{EventId, Run, RunId, RunOutput, RunStatus, Timestamp};
use rusqlite::types::Value as Sql;
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};
use serde_json::json;

use crate::events::parse_id;
use crate::outbox::{self, Effect};
use crate::{Queries, Result, Store, StoreError};

/// A run created by a routing decision (or a rerun). Status is `Queued` or `PendingApproval`.
#[derive(Clone, Debug, PartialEq)]
pub struct NewRun {
    pub id: RunId,
    pub rule: Option<String>,
    pub agent: String,
    pub event: EventId,
    pub status: RunStatus,
    pub created_at: Timestamp,
    pub prompt: Option<String>,
    pub chain_depth: u32,
    pub rerun_of: Option<RunId>,
    pub snapshot: RunSnapshot,
    /// What the run will execute, rendered from the snapshot when it is created.
    pub invocation: Option<InvocationSummary>,
}

/// A requested state change, with the data that goes with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunTransition {
    /// `pending_approval → queued`; also enqueues the run's start in the outbox.
    Approve,
    /// `pending_approval → rejected`.
    Reject { reason: Option<String> },
    /// `pending_approval | queued → cancelled`. A running run is cancelled by signalling its
    /// process; the supervisor's result is then recorded with `Finish { status: Cancelled }`.
    Cancel,
    /// `pending_approval | queued → cancelled` by the daemon, with the reason: the run can no
    /// longer start as created (its agent or rule changed).
    Withdraw { reason: String },
    /// `queued → running`. Recorded before the process is spawned (`pid` is set once known,
    /// [`Store::set_run_pid`]), so a crash in between leaves a `running` run that recovery marks
    /// `interrupted`, never a `queued` one that would start a second copy.
    Start { pid: Option<u32>, invocation: InvocationSummary },
    /// `queued → failed`: the process could not be spawned.
    FailToStart { error: String },
    /// `running → succeeded | failed | timed_out | cancelled | interrupted` (interrupted: the
    /// daemon shut down and terminated the run).
    Finish { status: RunStatus, exit_code: Option<i32>, signal: Option<i32>, error: Option<String> },
}

impl RunTransition {
    fn target(&self) -> RunStatus {
        match self {
            RunTransition::Approve => RunStatus::Queued,
            RunTransition::Reject { .. } => RunStatus::Rejected,
            RunTransition::Cancel | RunTransition::Withdraw { .. } => RunStatus::Cancelled,
            RunTransition::Start { .. } => RunStatus::Running,
            RunTransition::FailToStart { .. } => RunStatus::Failed,
            RunTransition::Finish { status, .. } => *status,
        }
    }
}

pub(crate) fn insert(tx: &Transaction<'_>, r: &NewRun) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO runs (id, rule, agent, event_id, status, created_at, prompt, chain_depth, rerun_of, snapshot, invocation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?
    .execute(params![
        r.id.to_string(),
        r.rule,
        r.agent,
        r.event.to_string(),
        r.status.as_str(),
        r.created_at.as_millis(),
        r.prompt,
        r.chain_depth,
        r.rerun_of.map(|x| x.to_string()),
        serde_json::to_string(&r.snapshot)?,
        r.invocation.as_ref().map(serde_json::to_string).transpose()?,
    ])?;
    Ok(())
}

const RUN_COLUMNS: &str = "id, rule, agent, event_id, status, created_at, started_at, ended_at, prompt, invocation, \
     pid, exit_code, signal, error, chain_depth, rerun_of, snapshot, output_bytes, output_dropped";

const SUMMARY_COLUMNS: &str =
    "id, rule, agent, event_id, status, created_at, started_at, ended_at, exit_code, chain_depth";

fn status(s: &str) -> Result<RunStatus> {
    RunStatus::parse(s).ok_or_else(|| StoreError::Corrupt(format!("bad run status {s:?}")))
}

fn ts(v: Option<i64>) -> Option<Timestamp> {
    v.map(Timestamp::from_millis)
}

fn read_run(row: &rusqlite::Row<'_>) -> Result<Run> {
    let invocation: Option<String> = row.get(9)?;
    let snapshot: String = row.get(16)?;
    Ok(Run {
        id: parse_id(&row.get::<_, String>(0)?)?,
        rule: row.get(1)?,
        agent: row.get(2)?,
        event: parse_id(&row.get::<_, String>(3)?)?,
        status: status(&row.get::<_, String>(4)?)?,
        created_at: Timestamp::from_millis(row.get(5)?),
        started_at: ts(row.get(6)?),
        ended_at: ts(row.get(7)?),
        prompt: row.get(8)?,
        invocation: invocation.as_deref().map(serde_json::from_str).transpose()?,
        pid: row.get(10)?,
        exit_code: row.get(11)?,
        signal: row.get(12)?,
        error: row.get(13)?,
        chain_depth: row.get(14)?,
        rerun_of: row.get::<_, Option<String>>(15)?.as_deref().map(parse_id).transpose()?,
        output: OutputStats { bytes: row.get(17)?, dropped_bytes: row.get(18)? },
        snapshot: serde_json::from_str(&snapshot)?,
    })
}

fn read_summary(row: &rusqlite::Row<'_>) -> Result<RunSummary> {
    Ok(RunSummary {
        id: parse_id(&row.get::<_, String>(0)?)?,
        rule: row.get(1)?,
        agent: row.get(2)?,
        event: parse_id(&row.get::<_, String>(3)?)?,
        status: status(&row.get::<_, String>(4)?)?,
        created_at: Timestamp::from_millis(row.get(5)?),
        started_at: ts(row.get(6)?),
        ended_at: ts(row.get(7)?),
        exit_code: row.get(8)?,
        chain_depth: row.get(9)?,
    })
}

fn get_run(conn: &rusqlite::Connection, id: RunId) -> Result<Option<Run>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?1"))?;
    let mut rows = stmt.query([id.to_string()])?;
    match rows.next()? {
        Some(row) => Ok(Some(read_run(row)?)),
        None => Ok(None),
    }
}

impl Store {
    /// Applies a transition if the state machine allows it from the run's current status.
    pub fn transition_run(&mut self, id: RunId, t: RunTransition, now: Timestamp) -> Result<Run> {
        let tx = self.conn.transaction()?;
        let current: Option<String> = tx
            .prepare_cached("SELECT status FROM runs WHERE id = ?1")?
            .query_row([id.to_string()], |r| r.get(0))
            .optional()?;
        let from = status(
            &current.ok_or_else(|| StoreError::NotFound { what: "run", id: id.to_string() })?,
        )?;
        let to = t.target();
        use RunStatus::*;
        let from_ok = match &t {
            RunTransition::Approve | RunTransition::Reject { .. } => from == PendingApproval,
            RunTransition::Cancel | RunTransition::Withdraw { .. } => {
                matches!(from, PendingApproval | Queued)
            }
            RunTransition::Start { .. } | RunTransition::FailToStart { .. } => from == Queued,
            RunTransition::Finish { status, .. } => {
                from == Running
                    && matches!(status, Succeeded | Failed | TimedOut | Cancelled | Interrupted)
            }
        };
        let allowed = from_ok && from.can_transition_to(to);
        if !allowed {
            return Err(StoreError::IllegalTransition {
                run: id,
                from: from.as_str(),
                to: to.as_str(),
            });
        }
        let ms = now.as_millis();
        let rid = id.to_string();
        match &t {
            RunTransition::Approve => {
                tx.execute("UPDATE runs SET status = ?2 WHERE id = ?1", params![rid, to.as_str()])?;
                outbox::insert(&tx, &Effect::StartRun { run: id }, now)?;
            }
            RunTransition::Reject { reason } => {
                tx.execute(
                    "UPDATE runs SET status = ?2, ended_at = ?3, error = ?4 WHERE id = ?1",
                    params![rid, to.as_str(), ms, reason],
                )?;
            }
            RunTransition::Cancel => {
                tx.execute(
                    "UPDATE runs SET status = ?2, ended_at = ?3 WHERE id = ?1",
                    params![rid, to.as_str(), ms],
                )?;
            }
            RunTransition::Start { pid, invocation } => {
                tx.execute(
                    "UPDATE runs SET status = ?2, started_at = ?3, pid = ?4, invocation = ?5 WHERE id = ?1",
                    params![rid, to.as_str(), ms, pid, serde_json::to_string(invocation)?],
                )?;
            }
            RunTransition::Withdraw { reason: error } | RunTransition::FailToStart { error } => {
                tx.execute(
                    "UPDATE runs SET status = ?2, ended_at = ?3, error = ?4 WHERE id = ?1",
                    params![rid, to.as_str(), ms, error],
                )?;
            }
            RunTransition::Finish { status: _, exit_code, signal, error } => {
                tx.execute(
                    "UPDATE runs SET status = ?2, ended_at = ?3, exit_code = ?4, signal = ?5, error = ?6 WHERE id = ?1",
                    params![rid, to.as_str(), ms, exit_code, signal, error],
                )?;
            }
        }
        let run = get_run(&tx, id)?
            .ok_or_else(|| StoreError::NotFound { what: "run", id: rid.clone() })?;
        tx.commit()?;
        Ok(run)
    }

    /// Records the pid of a run's process once it is known (the run is already `running`).
    pub fn set_run_pid(&mut self, id: RunId, pid: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET pid = ?2 WHERE id = ?1 AND status = 'running'",
            params![id.to_string(), pid],
        )?;
        Ok(())
    }

    /// At startup: runs left `running` by a daemon that died become `interrupted` (their
    /// processes are not ours any more), and claimed outbox rows return to pending. The effect
    /// worker must check a run is still `queued` before starting it.
    pub fn recover_after_restart(&mut self, now: Timestamp) -> Result<Vec<RunId>> {
        let tx = self.conn.transaction()?;
        let ids: Vec<String> = tx
            .prepare("SELECT id FROM runs WHERE status = 'running'")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        tx.execute(
            "UPDATE runs SET status = 'interrupted', ended_at = ?1, error = 'the daemon stopped while the run was active'
             WHERE status = 'running'",
            [now.as_millis()],
        )?;
        tx.execute(
            "UPDATE outbox SET status = 'pending', claimed_at = NULL WHERE status = 'claimed'",
            [],
        )?;
        tx.commit()?;
        ids.iter().map(|s| parse_id(s)).collect()
    }

    /// Appends output chunks to a run, keeping at most `cap` bytes: the first `cap / 2` bytes
    /// (the head), then the most recent output (the tail). Once output is dropped, a `marker`
    /// chunk between head and tail says how much. A single chunk larger than half the cap is cut.
    pub fn append_output(
        &mut self,
        run: RunId,
        chunks: &[(Timestamp, RunOutput)],
        cap: u64,
    ) -> Result<OutputStats> {
        let tx = self.conn.transaction()?;
        let rid = run.to_string();
        let (mut total, mut dropped, mut kept, mut head_end, mut next): (u64, u64, u64, Option<u64>, u64) = tx
            .prepare_cached(
                "SELECT output_bytes, output_dropped, output_kept, output_head_end, output_next_seq FROM runs WHERE id = ?1",
            )?
            .query_row([&rid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .optional()?
            .ok_or_else(|| StoreError::NotFound { what: "run", id: rid.clone() })?;
        let half = (cap / 2).max(1);
        for (at, out) in chunks {
            let data = out.data.as_ref().map(serde_json::to_string).transpose()?;
            let mut text = out.text.clone();
            let mut bytes = (text.len() + data.as_ref().map_or(0, String::len)) as u64;
            total += bytes;
            if bytes > half {
                let mut end = (half as usize).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let cut = text.len() - end;
                text.truncate(end);
                text.push('…');
                dropped += cut as u64;
                bytes = (text.len() + data.as_ref().map_or(0, String::len)) as u64;
            }
            if head_end.is_none() && kept + bytes > half {
                // Close the head; reserve the next seq for the marker.
                head_end = Some(next - 1);
                next += 1;
            }
            let seq = next;
            next += 1;
            tx.prepare_cached(
                "INSERT INTO run_output (run_id, seq, at, kind, text, data, bytes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![rid, seq, at.as_millis(), out.kind.as_str(), text, data, bytes])?;
            kept += bytes;
            if let Some(h) = head_end {
                let marker = h + 1;
                while kept > cap {
                    let oldest: Option<(u64, u64)> = tx
                        .prepare_cached(
                            "SELECT seq, bytes FROM run_output WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT 1",
                        )?
                        .query_row(params![rid, marker], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()?;
                    let Some((s, b)) = oldest else { break };
                    tx.execute(
                        "DELETE FROM run_output WHERE run_id = ?1 AND seq = ?2",
                        params![rid, s],
                    )?;
                    kept -= b;
                    dropped += b;
                }
                if dropped > 0 {
                    tx.prepare_cached(
                        "INSERT INTO run_output (run_id, seq, at, kind, text, data, bytes) VALUES (?1, ?2, ?3, 'marker', ?4, ?5, 0)
                         ON CONFLICT (run_id, seq) DO UPDATE SET text = excluded.text, data = excluded.data, at = excluded.at",
                    )?
                    .execute(params![
                        rid,
                        marker,
                        at.as_millis(),
                        format!("[… {dropped} bytes of output dropped …]"),
                        json!({ "dropped_bytes": dropped }).to_string(),
                    ])?;
                }
            }
        }
        tx.execute(
            "UPDATE runs SET output_bytes = ?2, output_dropped = ?3, output_kept = ?4, output_head_end = ?5, output_next_seq = ?6
             WHERE id = ?1",
            params![rid, total, dropped, kept, head_end, next],
        )?;
        tx.commit()?;
        Ok(OutputStats { bytes: total, dropped_bytes: dropped })
    }
}

impl Queries<'_> {
    pub fn run(&self, id: RunId) -> Result<Option<Run>> {
        get_run(self.conn, id)
    }

    /// Runs that ended at or after `since`, newest first, at most `limit`. The daemon rebuilds
    /// inferred causation from these at startup.
    pub fn runs_ended_since(&self, since: Timestamp, limit: u32) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM runs WHERE ended_at >= ?1 ORDER BY ended_at DESC LIMIT ?2"
        ))?;
        let mut rows = stmt.query(params![since.as_millis(), limit])?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next()? {
            runs.push(read_run(row)?);
        }
        Ok(runs)
    }

    /// Runs matching a filter, newest first.
    pub fn list_runs(
        &self,
        filter: &RunFilter,
        cursor: Option<RunId>,
        limit: Option<u32>,
    ) -> Result<RunPage> {
        let mut clauses = Vec::new();
        let mut args = Vec::new();
        if let Some(rule) = &filter.rule {
            clauses.push("rule = ?".to_owned());
            args.push(Sql::Text(rule.clone()));
        }
        if let Some(agent) = &filter.agent {
            clauses.push("agent = ?".to_owned());
            args.push(Sql::Text(agent.clone()));
        }
        if !filter.status.is_empty() {
            clauses.push(format!("status IN ({})", vec!["?"; filter.status.len()].join(", ")));
            args.extend(filter.status.iter().map(|s| Sql::Text(s.as_str().to_owned())));
        }
        if let Some(event) = filter.event {
            clauses.push("event_id = ?".to_owned());
            args.push(Sql::Text(event.to_string()));
        }
        if let Some(c) = cursor {
            clauses.push("id < ?".to_owned());
            args.push(Sql::Text(c.to_string()));
        }
        let limit = page_limit(limit);
        let where_ = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SUMMARY_COLUMNS} FROM runs {where_} ORDER BY id DESC LIMIT {}",
            limit + 1
        ))?;
        let mut rows = stmt.query(params_from_iter(args))?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next()? {
            runs.push(read_summary(row)?);
        }
        let next_cursor = if runs.len() > limit as usize {
            runs.truncate(limit as usize);
            runs.last().map(|r| r.id)
        } else {
            None
        };
        Ok(RunPage { runs, next_cursor })
    }

    /// Output chunks with `seq > after`, in order.
    pub fn run_output(
        &self,
        id: RunId,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<RunOutputPage> {
        let run = self
            .run(id)?
            .ok_or_else(|| StoreError::NotFound { what: "run", id: id.to_string() })?;
        let limit = page_limit(limit);
        let mut stmt = self.conn.prepare_cached(
            "SELECT seq, at, kind, text, data FROM run_output WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let mut rows = stmt.query(params![id.to_string(), after.unwrap_or(0), limit])?;
        let mut chunks = Vec::new();
        while let Some(row) = rows.next()? {
            let kind: String = row.get(2)?;
            let data: Option<String> = row.get(4)?;
            chunks.push(RunOutputChunk {
                seq: row.get(0)?,
                at: Timestamp::from_millis(row.get(1)?),
                output: RunOutput {
                    kind: OutputKind::parse(&kind)
                        .ok_or_else(|| StoreError::Corrupt(format!("bad output kind {kind:?}")))?,
                    text: row.get(3)?,
                    data: data.as_deref().map(serde_json::from_str).transpose()?,
                },
            });
        }
        let next_after = chunks.last().map(|c| c.seq).or(after);
        Ok(RunOutputPage { chunks, next_after, stats: run.output, done: run.status.is_terminal() })
    }

    pub fn runs_for_event(&self, event: EventId) -> Result<Vec<RunSummary>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {SUMMARY_COLUMNS} FROM runs WHERE event_id = ?1 ORDER BY id"
        ))?;
        let mut rows = stmt.query([event.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(read_summary(row)?);
        }
        Ok(out)
    }

    pub fn run_counts(&self) -> Result<RunCounts> {
        let mut counts = RunCounts::default();
        let mut stmt = self.conn.prepare_cached(
            "SELECT status, COUNT(*) FROM runs WHERE status IN ('running', 'queued', 'pending_approval') GROUP BY status",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let n: u32 = row.get(1)?;
            match row.get::<_, String>(0)?.as_str() {
                "running" => counts.running = n,
                "queued" => counts.queued = n,
                _ => counts.pending_approval = n,
            }
        }
        Ok(counts)
    }

    /// Queued plus running runs per agent (the router's backlog).
    pub fn agent_backlog(&self) -> Result<BTreeMap<String, u32>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT agent, COUNT(*) FROM runs WHERE status IN ('running', 'queued') GROUP BY agent",
        )?;
        let rows =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
}
