//! Retention: pruning in bounded batches, off the hot path.

use pulse_contracts::Timestamp;
use pulse_contracts::config::Retention;
use rusqlite::params;

use crate::{Result, Store};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub runs: usize,
    pub events: usize,
    /// A batch was full: call again (after yielding) to continue.
    pub more: bool,
}

impl Store {
    /// Deletes at most `batch` runs and `batch` events per call:
    /// - terminal runs that ended before `now - runs_max_age`, with their output;
    /// - events older than `events_max_age`, then the oldest events beyond `events_max_count`,
    ///   skipping any event a remaining run refers to. Their paths and deliveries go with them.
    pub fn prune(&mut self, policy: &Retention, now: Timestamp, batch: u32) -> Result<PruneReport> {
        let tx = self.conn.transaction()?;
        let run_cutoff = now.saturating_sub(policy.runs_max_age).as_millis();
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS prune_ids (id TEXT PRIMARY KEY) WITHOUT ROWID; DELETE FROM prune_ids;")?;
        tx.execute(
            "INSERT INTO prune_ids SELECT id FROM runs
             WHERE status NOT IN ('pending_approval', 'queued', 'running') AND COALESCE(ended_at, created_at) < ?1
             ORDER BY id LIMIT ?2",
            params![run_cutoff, batch],
        )?;
        tx.execute("DELETE FROM run_output WHERE run_id IN (SELECT id FROM prune_ids)", [])?;
        tx.execute(
            "UPDATE deliveries SET run_id = NULL WHERE run_id IN (SELECT id FROM prune_ids)",
            [],
        )?;
        let runs = tx.execute("DELETE FROM runs WHERE id IN (SELECT id FROM prune_ids)", [])?;
        tx.execute("DELETE FROM prune_ids", [])?;

        let event_cutoff = now.saturating_sub(policy.events_max_age).as_millis();
        tx.execute(
            "INSERT INTO prune_ids SELECT id FROM events
             WHERE time < ?1 AND id NOT IN (SELECT event_id FROM runs)
             ORDER BY id LIMIT ?2",
            params![event_cutoff, batch],
        )?;
        let by_age: u32 = tx.query_row("SELECT COUNT(*) FROM prune_ids", [], |r| r.get(0))?;
        let total: u64 = tx.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        let excess = total.saturating_sub(by_age as u64).saturating_sub(policy.events_max_count);
        let room = (batch - by_age.min(batch)) as u64;
        if excess > 0 && room > 0 {
            tx.execute(
                "INSERT OR IGNORE INTO prune_ids SELECT id FROM events
                 WHERE id NOT IN (SELECT event_id FROM runs) AND id NOT IN (SELECT id FROM prune_ids)
                 ORDER BY id LIMIT ?1",
                [excess.min(room)],
            )?;
        }
        tx.execute("DELETE FROM event_paths WHERE event_id IN (SELECT id FROM prune_ids)", [])?;
        tx.execute("DELETE FROM deliveries WHERE event_id IN (SELECT id FROM prune_ids)", [])?;
        let events = tx.execute("DELETE FROM events WHERE id IN (SELECT id FROM prune_ids)", [])?;
        tx.execute("DELETE FROM prune_ids", [])?;
        tx.commit()?;
        let more = runs as u32 >= batch || events as u32 >= batch;
        Ok(PruneReport { runs, events, more })
    }
}
