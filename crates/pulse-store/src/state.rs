//! Rule runtime state and small key-value flags.

use pulse_contracts::Timestamp;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::{Queries, Result, Store};

/// Persisted runtime state of one rule. Fire history is not stored here; it is rebuilt from
/// deliveries ([`Queries::recent_fires`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleStateRow {
    pub rule: String,
    pub snoozed_until: Option<Timestamp>,
    pub circuit_open_until: Option<Timestamp>,
    pub last_fired_at: Option<Timestamp>,
}

pub(crate) fn upsert_rule_state(tx: &Transaction<'_>, s: &RuleStateRow) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO rule_state (rule, snoozed_until, circuit_open_until, last_fired_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (rule) DO UPDATE SET snoozed_until = excluded.snoozed_until,
             circuit_open_until = excluded.circuit_open_until, last_fired_at = excluded.last_fired_at",
    )?
    .execute(params![
        s.rule,
        s.snoozed_until.map(Timestamp::as_millis),
        s.circuit_open_until.map(Timestamp::as_millis),
        s.last_fired_at.map(Timestamp::as_millis),
    ])?;
    Ok(())
}

/// Well-known flag keys.
pub mod flags {
    /// `"1"` while the global pause is on.
    pub const PAUSED: &str = "paused";
}

impl Store {
    pub fn set_rule_state(&mut self, s: &RuleStateRow) -> Result<()> {
        let tx = self.conn.transaction()?;
        upsert_rule_state(&tx, s)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_rule_state(&mut self, rule: &str) -> Result<()> {
        self.conn.execute("DELETE FROM rule_state WHERE rule = ?1", [rule])?;
        Ok(())
    }

    pub fn set_flag(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_flag(&mut self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
        Ok(())
    }
}

impl Queries<'_> {
    pub fn rule_states(&self) -> Result<Vec<RuleStateRow>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT rule, snoozed_until, circuit_open_until, last_fired_at FROM rule_state ORDER BY rule")?;
        let ts = |v: Option<i64>| v.map(Timestamp::from_millis);
        let rows = stmt
            .query_map([], |r| {
                Ok(RuleStateRow {
                    rule: r.get(0)?,
                    snoozed_until: ts(r.get(1)?),
                    circuit_open_until: ts(r.get(2)?),
                    last_fired_at: ts(r.get(3)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn flag(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .prepare_cached("SELECT value FROM kv WHERE key = ?1")?
            .query_row([key], |r| r.get(0))
            .optional()?)
    }
}
