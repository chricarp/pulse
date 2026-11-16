//! The outbox: effects recorded with the decision that caused them, performed later by the
//! daemon's effect worker.

use pulse_contracts::{RunId, Timestamp};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{Queries, Result, Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// Start a queued run. The worker must check the run is still `queued`.
    StartRun { run: RunId },
}

impl Effect {
    fn run(&self) -> Option<RunId> {
        match self {
            Effect::StartRun { run } => Some(*run),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxItem {
    pub id: i64,
    pub effect: Effect,
    /// Including this one.
    pub attempts: u32,
}

pub(crate) fn insert(tx: &Transaction<'_>, effect: &Effect, available_at: Timestamp) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO outbox (effect, run_id, status, available_at) VALUES (?1, ?2, 'pending', ?3)",
    )?
    .execute(params![
        serde_json::to_string(effect)?,
        effect.run().map(|r| r.to_string()),
        available_at.as_millis()
    ])?;
    Ok(())
}

impl Store {
    /// Claims up to `limit` pending effects that are due, oldest first.
    pub fn claim_outbox(&mut self, now: Timestamp, limit: u32) -> Result<Vec<OutboxItem>> {
        let mut stmt = self.conn.prepare_cached(
            "UPDATE outbox SET status = 'claimed', claimed_at = ?1, attempts = attempts + 1
             WHERE id IN (SELECT id FROM outbox WHERE status = 'pending' AND available_at <= ?1 ORDER BY id LIMIT ?2)
             RETURNING id, effect, attempts",
        )?;
        let rows: Vec<(i64, String, u32)> = stmt
            .query_map(params![now.as_millis(), limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut items = rows
            .into_iter()
            .map(|(id, effect, attempts)| {
                Ok(OutboxItem { id, effect: serde_json::from_str(&effect)?, attempts })
            })
            .collect::<Result<Vec<_>>>()?;
        items.sort_by_key(|i| i.id);
        Ok(items)
    }

    /// The effect was performed: the row is removed.
    pub fn complete_outbox(&mut self, id: i64) -> Result<()> {
        let n =
            self.conn.execute("DELETE FROM outbox WHERE id = ?1 AND status = 'claimed'", [id])?;
        if n == 0 {
            return Err(StoreError::NotFound { what: "claimed outbox item", id: id.to_string() });
        }
        Ok(())
    }

    /// The effect failed. With `retry_at` it becomes pending again from then; without, it is
    /// parked as `failed` and kept for inspection.
    pub fn fail_outbox(&mut self, id: i64, error: &str, retry_at: Option<Timestamp>) -> Result<()> {
        let n = match retry_at {
            Some(at) => self.conn.execute(
                "UPDATE outbox SET status = 'pending', claimed_at = NULL, last_error = ?2, available_at = ?3
                 WHERE id = ?1 AND status = 'claimed'",
                params![id, error, at.as_millis()],
            )?,
            None => self.conn.execute(
                "UPDATE outbox SET status = 'failed', last_error = ?2 WHERE id = ?1 AND status = 'claimed'",
                params![id, error],
            )?,
        };
        if n == 0 {
            return Err(StoreError::NotFound { what: "claimed outbox item", id: id.to_string() });
        }
        Ok(())
    }
}

impl Queries<'_> {
    /// When the next pending effect becomes due, so the worker can arm one timer.
    pub fn next_outbox_at(&self) -> Result<Option<Timestamp>> {
        let at: Option<i64> = self
            .conn
            .prepare_cached("SELECT MIN(available_at) FROM outbox WHERE status = 'pending'")?
            .query_row([], |r| r.get(0))
            .optional()?
            .flatten();
        Ok(at.map(Timestamp::from_millis))
    }
}
