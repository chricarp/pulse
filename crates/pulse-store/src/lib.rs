//! SQLite persistence: the event log, deliveries, runs and their output, the outbox, rule
//! runtime state, flags, retention. It stores what it is told; it makes no decisions.
//!
//! ## Threading model
//!
//! [`Store`] is a synchronous API over the one **writer** connection. [`Reader`] is a read-only
//! connection (WAL lets it read while the writer writes). Neither is `Sync`.
//!
//! Async code uses [`StoreHandle`]: it owns two dedicated threads, one with the `Store` and
//! one with a `Reader`, each fed by a bounded channel. `handle.write(|s| s.commit(&batch))` and
//! `handle.read(|r| r.query().event(id))` send a closure and await its result, so the runtime
//! never blocks on SQLite, writes are serialized without locks, and a long query does not hold
//! up the writer. A full channel makes callers wait (backpressure), never buffer.
//!
//! ## Guarantees
//!
//! - [`Store::commit`] writes events, deliveries, runs, outbox rows and rule state in **one
//!   transaction**: a routing result is all there or not at all.
//! - Run status changes go through [`Store::transition_run`], which rejects illegal transitions
//!   (`RunStatus::can_transition_to`). Approval writes the outbox row in the same transaction.
//! - Output is capped per run, keeping the head and the tail with a marker chunk in between.
//! - Retention deletes in bounded batches and never deletes an event a retained run refers to.
//!
//! Tests use temporary directories or in-memory databases, never a real Pulse home.

mod events;
mod handle;
mod outbox;
mod retention;
mod runs;
mod schema;
mod state;

use std::path::Path;

use pulse_contracts::{Delivery, Event, RunId};
use rusqlite::{Connection, OpenFlags};

pub use handle::{Queryable, StoreHandle};
pub use outbox::{Effect, OutboxItem};
pub use retention::PruneReport;
pub use runs::{NewRun, RunTransition};
pub use state::{RuleStateRow, flags};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration {version} failed: {message}")]
    Migration { version: u32, message: String },
    #[error(
        "the database is at schema version {found}, newer than this Pulse ({known}); upgrade Pulse"
    )]
    TooNew { found: u32, known: u32 },
    #[error("{what} {id} not found")]
    NotFound { what: &'static str, id: String },
    #[error("run {run}: cannot go from {from} to {to}")]
    IllegalTransition { run: RunId, from: &'static str, to: &'static str },
    /// The query itself is invalid (for example an unclosed `{` in a subject glob).
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("stored data is unreadable: {0}")]
    Corrupt(String),
    #[error("the store has shut down")]
    Closed,
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Corrupt(e.to_string())
    }
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

/// The writer.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the database at `path`, switches it to WAL and migrates it.
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Store::init(conn)
    }

    /// A private in-memory database, for tests.
    pub fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Store> {
        configure(&conn)?;
        schema::migrate(&mut conn)?;
        Ok(Store { conn })
    }

    pub fn schema_version(&self) -> Result<u32> {
        schema::version(&self.conn)
    }

    pub fn query(&self) -> Queries<'_> {
        Queries { conn: &self.conn }
    }

    /// Flushes the WAL into the main file. Call at shutdown.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Appends events in one transaction (events with no routing result to record).
    pub fn append_events(&mut self, events: &[Event]) -> Result<()> {
        self.commit(&Commit { events: events.to_vec(), ..Commit::default() })
    }

    /// Writes one routing result atomically.
    pub fn commit(&mut self, batch: &Commit) -> Result<()> {
        let tx = self.conn.transaction()?;
        for e in &batch.events {
            events::insert(&tx, e)?;
        }
        for r in &batch.runs {
            runs::insert(&tx, r)?;
        }
        for d in &batch.deliveries {
            events::insert_delivery(&tx, d)?;
        }
        for effect in &batch.outbox {
            outbox::insert(&tx, effect, batch.now)?;
        }
        for s in &batch.rule_states {
            state::upsert_rule_state(&tx, s)?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// Everything one routing step produces, written by [`Store::commit`] in one transaction.
#[derive(Clone, Debug, Default)]
pub struct Commit {
    /// Used as the outbox rows' `available_at`.
    pub now: pulse_contracts::Timestamp,
    pub events: Vec<Event>,
    pub deliveries: Vec<Delivery>,
    pub runs: Vec<NewRun>,
    pub outbox: Vec<Effect>,
    pub rule_states: Vec<RuleStateRow>,
}

/// A read-only connection.
#[derive(Debug)]
pub struct Reader {
    conn: Connection,
}

impl Reader {
    /// Opens a read-only connection to a database a [`Store`] has already created.
    pub fn open(path: &Path) -> Result<Reader> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        configure(&conn)?;
        Ok(Reader { conn })
    }

    pub fn query(&self) -> Queries<'_> {
        Queries { conn: &self.conn }
    }
}

/// Read queries, available on both the writer and a reader.
#[derive(Clone, Copy, Debug)]
pub struct Queries<'c> {
    conn: &'c Connection,
}

#[cfg(test)]
mod tests;
