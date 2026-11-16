//! Numbered, append-only migrations. `PRAGMA user_version` holds the number applied.
//!
//! Never edit a migration that has shipped; add a new one at the end.

use rusqlite::Connection;

use crate::StoreError;

pub(crate) const MIGRATIONS: &[&str] = &[
    // 1: the v1 schema.
    r#"
CREATE TABLE events (
    id            TEXT PRIMARY KEY,
    type          TEXT NOT NULL,
    source        TEXT NOT NULL,
    subject       TEXT,
    time          INTEGER NOT NULL,
    causation_run TEXT,
    chain_depth   INTEGER NOT NULL,
    body          TEXT NOT NULL
) WITHOUT ROWID;
CREATE INDEX events_time ON events (time);
CREATE INDEX events_type ON events (type, id);
CREATE INDEX events_source ON events (source, id);
CREATE INDEX events_subject ON events (subject);
CREATE INDEX events_causation ON events (causation_run) WHERE causation_run IS NOT NULL;

CREATE TABLE event_paths (
    path     TEXT NOT NULL,
    event_id TEXT NOT NULL,
    PRIMARY KEY (path, event_id)
) WITHOUT ROWID;
CREATE INDEX event_paths_event ON event_paths (event_id);

CREATE TABLE deliveries (
    id       TEXT PRIMARY KEY,
    event_id TEXT NOT NULL,
    rule     TEXT NOT NULL,
    agent    TEXT NOT NULL,
    at       INTEGER NOT NULL,
    outcome  TEXT NOT NULL,
    detail   TEXT NOT NULL,
    run_id   TEXT
) WITHOUT ROWID;
CREATE INDEX deliveries_event ON deliveries (event_id);
CREATE INDEX deliveries_rule ON deliveries (rule, id);

CREATE TABLE runs (
    id              TEXT PRIMARY KEY,
    rule            TEXT,
    agent           TEXT NOT NULL,
    event_id        TEXT NOT NULL,
    status          TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    started_at      INTEGER,
    ended_at        INTEGER,
    prompt          TEXT,
    invocation      TEXT,
    pid             INTEGER,
    exit_code       INTEGER,
    signal          INTEGER,
    error           TEXT,
    chain_depth     INTEGER NOT NULL,
    rerun_of        TEXT,
    snapshot        TEXT NOT NULL,
    output_bytes    INTEGER NOT NULL DEFAULT 0,
    output_dropped  INTEGER NOT NULL DEFAULT 0,
    output_kept     INTEGER NOT NULL DEFAULT 0,
    output_head_end INTEGER,
    output_next_seq INTEGER NOT NULL DEFAULT 1
) WITHOUT ROWID;
CREATE INDEX runs_status ON runs (status, id);
CREATE INDEX runs_rule ON runs (rule, id);
CREATE INDEX runs_agent ON runs (agent, id);
CREATE INDEX runs_event ON runs (event_id);

CREATE TABLE run_output (
    run_id TEXT NOT NULL,
    seq    INTEGER NOT NULL,
    at     INTEGER NOT NULL,
    kind   TEXT NOT NULL,
    text   TEXT NOT NULL,
    data   TEXT,
    bytes  INTEGER NOT NULL,
    PRIMARY KEY (run_id, seq)
) WITHOUT ROWID;

CREATE TABLE outbox (
    id           INTEGER PRIMARY KEY,
    effect       TEXT NOT NULL,
    run_id       TEXT,
    status       TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    available_at INTEGER NOT NULL,
    claimed_at   INTEGER,
    last_error   TEXT
);
CREATE INDEX outbox_pending ON outbox (status, available_at, id);

CREATE TABLE rule_state (
    rule               TEXT PRIMARY KEY,
    snoozed_until      INTEGER,
    circuit_open_until INTEGER,
    last_fired_at      INTEGER
) WITHOUT ROWID;

CREATE TABLE kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
"#,
];

pub(crate) fn version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))?)
}

/// Applies migrations after the current version, each in its own transaction.
pub(crate) fn migrate(conn: &mut Connection) -> Result<u32, StoreError> {
    migrate_to(conn, MIGRATIONS.len())
}

pub(crate) fn migrate_to(conn: &mut Connection, target: usize) -> Result<u32, StoreError> {
    let current = version(conn)? as usize;
    if current > MIGRATIONS.len() {
        return Err(StoreError::TooNew { found: current as u32, known: MIGRATIONS.len() as u32 });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().take(target).skip(current) {
        let n = i + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)
            .map_err(|e| StoreError::Migration { version: n as u32, message: e.to_string() })?;
        tx.pragma_update(None, "user_version", n as u32)?;
        tx.commit()?;
    }
    version(conn)
}
