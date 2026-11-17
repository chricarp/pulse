//! The event log and deliveries.

use pulse_contracts::pattern::{GlobAlternative, GlobToken, parse_text_glob, parse_type_glob};
use pulse_contracts::rpc::{
    DeliveryKind, DeliveryPage, EventFilter, EventOutcome, EventPage, EventQuery, Order, page_limit,
};
use pulse_contracts::{
    Delivery, DeliveryId, DeliveryOutcome, Event, EventId, SourceKind, Timestamp,
};
use rusqlite::types::Value as Sql;
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};

use crate::{Queries, Result, StoreError};

pub(crate) fn insert(tx: &Transaction<'_>, e: &Event) -> Result<()> {
    let id = e.id.to_string();
    tx.prepare_cached(
        "INSERT INTO events (id, type, source, subject, time, causation_run, chain_depth, body)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        id,
        e.type_.as_str(),
        e.source,
        e.subject,
        e.time.as_millis(),
        e.causation_run.map(|r| r.to_string()),
        e.chain_depth,
        serde_json::to_string(e)?,
    ])?;
    let mut paths = e.paths();
    paths.sort_unstable();
    paths.dedup();
    let mut stmt =
        tx.prepare_cached("INSERT OR IGNORE INTO event_paths (path, event_id) VALUES (?1, ?2)")?;
    for p in paths {
        stmt.execute(params![p, id])?;
    }
    Ok(())
}

/// The newest event id in the log.
pub(crate) fn last_id(conn: &rusqlite::Connection) -> Result<Option<EventId>> {
    let id: Option<String> = conn
        .prepare_cached("SELECT id FROM events ORDER BY id DESC LIMIT 1")?
        .query_row([], |r| r.get(0))
        .optional()?;
    id.as_deref().map(parse_id).transpose()
}

pub(crate) fn insert_delivery(tx: &Transaction<'_>, d: &Delivery) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO deliveries (id, event_id, rule, agent, at, outcome, detail, run_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        d.id.to_string(),
        d.event.to_string(),
        d.rule,
        d.agent,
        d.at.as_millis(),
        d.outcome.kind_str(),
        serde_json::to_string(&d.outcome)?,
        d.run.map(|r| r.to_string()),
    ])?;
    Ok(())
}

pub(crate) fn parse_id<T: std::str::FromStr>(s: &str) -> Result<T> {
    s.parse().map_err(|_| StoreError::Corrupt(format!("bad id {s:?}")))
}

/// A delivery row as stored: id, event, rule, agent, at, detail JSON, run.
type DeliveryRow = (String, String, String, String, i64, String, Option<String>);

fn read_delivery(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
}

fn to_delivery(t: DeliveryRow) -> Result<Delivery> {
    let (id, event, rule, agent, at, detail, run) = t;
    Ok(Delivery {
        id: parse_id(&id)?,
        event: parse_id(&event)?,
        rule,
        agent,
        at: Timestamp::from_millis(at),
        outcome: serde_json::from_str::<DeliveryOutcome>(&detail)?,
        run: run.as_deref().map(parse_id).transpose()?,
    })
}

const DELIVERY_COLUMNS: &str = "id, event_id, rule, agent, at, detail, run_id";

/// Escapes a literal for SQLite `GLOB`.
fn glob_literal(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            '[' => out.push_str("[[]"),
            c => out.push(c),
        }
    }
}

/// Text glob alternatives as SQLite `GLOB` patterns.
fn sql_globs(alts: &[GlobAlternative]) -> Vec<String> {
    alts.iter()
        .filter_map(|a| match a {
            GlobAlternative::Text(tokens) => {
                let mut s = String::new();
                for t in tokens {
                    match t {
                        GlobToken::Literal(l) => glob_literal(l, &mut s),
                        GlobToken::Star => s.push('*'),
                        GlobToken::Any => s.push('?'),
                    }
                }
                Some(s)
            }
            GlobAlternative::Path(_) => None,
        })
        .collect()
}

fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('%');
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

const KINDS: [SourceKind; 6] = [
    SourceKind::Fs,
    SourceKind::Git,
    SourceKind::Schedule,
    SourceKind::Calendar,
    SourceKind::Process,
    SourceKind::Ext,
];

/// Builds the WHERE clause for a filter. Invalid globs are a query error, not "match nothing".
fn filter_sql(f: &EventFilter, sql: &mut Vec<String>, args: &mut Vec<Sql>) -> Result<()> {
    if !f.types.is_empty() {
        let mut ors = Vec::new();
        for t in &f.types {
            let g =
                parse_type_glob(t.as_str()).map_err(|e| StoreError::InvalidQuery(e.to_string()))?;
            for p in sql_globs(&g.alternatives) {
                ors.push("type GLOB ?".to_owned());
                args.push(Sql::Text(p));
            }
        }
        sql.push(format!("({})", ors.join(" OR ")));
    }
    if !f.sources.is_empty() {
        let mut uris = Vec::new();
        for s in &f.sources {
            if s.contains("://") {
                uris.push(s.clone());
            } else {
                uris.extend(KINDS.iter().map(|k| format!("pulse://{k}/{s}")));
            }
        }
        sql.push(format!("source IN ({})", vec!["?"; uris.len()].join(", ")));
        args.extend(uris.into_iter().map(Sql::Text));
    }
    if let Some(subject) = &f.subject {
        let g = parse_text_glob(subject).map_err(|e| StoreError::InvalidQuery(e.to_string()))?;
        let pats = sql_globs(&g.alternatives);
        sql.push(format!("({})", vec!["subject GLOB ?"; pats.len()].join(" OR ")));
        args.extend(pats.into_iter().map(Sql::Text));
    }
    if let Some(path) = &f.path {
        let dir = path.trim_end_matches('/');
        // Paths under `dir` sort between `dir/` and `dir0` ('0' follows '/').
        sql.push(
            "id IN (SELECT event_id FROM event_paths WHERE path = ? OR (path >= ? AND path < ?))"
                .to_owned(),
        );
        args.push(Sql::Text(if dir.is_empty() { "/".into() } else { dir.to_owned() }));
        args.push(Sql::Text(format!("{dir}/")));
        args.push(Sql::Text(format!("{dir}0")));
    }
    if let Some(text) = &f.text {
        sql.push("body LIKE ? ESCAPE '\\'".to_owned());
        args.push(Sql::Text(like_escape(text)));
    }
    if let Some(since) = f.since {
        sql.push("time >= ?".to_owned());
        args.push(Sql::Integer(since.as_millis()));
    }
    if let Some(until) = f.until {
        sql.push("time <= ?".to_owned());
        args.push(Sql::Integer(until.as_millis()));
    }
    if let Some(run) = f.caused_by {
        sql.push("causation_run = ?".to_owned());
        args.push(Sql::Text(run.to_string()));
    }
    Ok(())
}

impl Queries<'_> {
    pub fn event(&self, id: EventId) -> Result<Option<Event>> {
        let body: Option<String> = self
            .conn
            .prepare_cached("SELECT body FROM events WHERE id = ?1")?
            .query_row([id.to_string()], |r| r.get(0))
            .optional()?;
        body.map(|b| serde_json::from_str(&b).map_err(StoreError::from)).transpose()
    }

    /// The newest event id in the log: the daemon continues its id sequence after it.
    pub fn last_event_id(&self) -> Result<Option<EventId>> {
        last_id(self.conn)
    }

    /// Events matching a filter, by id (newest first by default), with cursor pagination.
    pub fn query_events(&self, q: &EventQuery) -> Result<EventPage> {
        let mut clauses = Vec::new();
        let mut args = Vec::new();
        filter_sql(&q.filter, &mut clauses, &mut args)?;
        let (cmp, dir) = match q.order {
            Order::NewestFirst => ("<", "DESC"),
            Order::OldestFirst => (">", "ASC"),
        };
        if let Some(cursor) = q.cursor {
            clauses.push(format!("id {cmp} ?"));
            args.push(Sql::Text(cursor.to_string()));
        }
        let limit = page_limit(q.limit);
        let where_ = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let sql = format!("SELECT body FROM events {where_} ORDER BY id {dir} LIMIT {}", limit + 1);
        let mut stmt = self.conn.prepare(&sql)?;
        let bodies: Vec<String> = stmt
            .query_map(params_from_iter(args), |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut events = bodies
            .iter()
            .map(|b| serde_json::from_str::<Event>(b))
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if events.len() > limit as usize {
            events.truncate(limit as usize);
            events.last().map(|e| e.id)
        } else {
            None
        };
        Ok(EventPage { events, next_cursor })
    }

    pub fn deliveries_for_event(&self, id: EventId) -> Result<Vec<Delivery>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {DELIVERY_COLUMNS} FROM deliveries WHERE event_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt
            .query_map([id.to_string()], read_delivery)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(to_delivery).collect()
    }

    /// Every delivery of these events, in the order they were recorded, with the current
    /// status of the run each created (if it is still in the log).
    pub fn outcomes_for_events(&self, ids: &[EventId]) -> Result<Vec<EventOutcome>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; ids.len()].join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.id, d.event_id, d.rule, d.agent, d.at, d.detail, d.run_id, r.status
             FROM deliveries d LEFT JOIN runs r ON r.id = d.run_id
             WHERE d.event_id IN ({marks}) ORDER BY d.id"
        ))?;
        let rows = stmt
            .query_map(params_from_iter(ids.iter().map(|i| i.to_string())), |row| {
                Ok((read_delivery(row)?, row.get::<_, Option<String>>(7)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(d, status)| {
                let status = status.as_deref().map(crate::runs::status).transpose()?;
                Ok(EventOutcome::new(&to_delivery(d)?, status))
            })
            .collect()
    }

    /// A rule's deliveries, newest first.
    pub fn rule_history(
        &self,
        rule: &str,
        outcomes: &[DeliveryKind],
        cursor: Option<DeliveryId>,
        limit: Option<u32>,
    ) -> Result<DeliveryPage> {
        let mut sql = format!("SELECT {DELIVERY_COLUMNS} FROM deliveries WHERE rule = ?");
        let mut args = vec![Sql::Text(rule.to_owned())];
        if !outcomes.is_empty() {
            sql.push_str(&format!(" AND outcome IN ({})", vec!["?"; outcomes.len()].join(", ")));
            args.extend(outcomes.iter().map(|o| {
                Sql::Text(
                    match o {
                        DeliveryKind::Fired => "fired",
                        DeliveryKind::AwaitingApproval => "awaiting_approval",
                        DeliveryKind::Debounced => "debounced",
                        DeliveryKind::Waiting => "waiting",
                        DeliveryKind::Blocked => "blocked",
                    }
                    .to_owned(),
                )
            }));
        }
        if let Some(c) = cursor {
            sql.push_str(" AND id < ?");
            args.push(Sql::Text(c.to_string()));
        }
        let limit = page_limit(limit);
        sql.push_str(&format!(" ORDER BY id DESC LIMIT {}", limit + 1));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(args), read_delivery)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut deliveries = rows.into_iter().map(to_delivery).collect::<Result<Vec<_>>>()?;
        let next_cursor = if deliveries.len() > limit as usize {
            deliveries.truncate(limit as usize);
            deliveries.last().map(|d| d.id)
        } else {
            None
        };
        Ok(DeliveryPage { deliveries, next_cursor, causes: Default::default() })
    }

    /// `(source, dedupe key, when accepted)` of events accepted at or after `since`, oldest
    /// first; at most `limit`, the newest kept. Event ids are ULIDs from the engine's clock, so
    /// "accepted at" is the id's time, whatever the event claims as its `time`. The daemon
    /// refills its dedupe table from this at startup, so a source retrying after an outage does
    /// not fire twice.
    pub fn recent_dedupe_keys(
        &self,
        since: Timestamp,
        limit: u32,
    ) -> Result<Vec<(String, String, Timestamp)>> {
        let floor = EventId::from_ulid(pulse_contracts::Ulid::from_parts(
            u64::try_from(since.as_millis()).unwrap_or(0),
            0,
        ));
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, source, json_extract(body, '$.pulsededupekey') AS k FROM events
             WHERE id >= ?1 AND k IS NOT NULL ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![floor.to_string(), limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut keys = rows
            .into_iter()
            .map(|(id, source, key)| {
                let id: EventId = parse_id(&id)?;
                let at = i64::try_from(id.timestamp_ms()).unwrap_or(i64::MAX);
                Ok((source, key, Timestamp::from_millis(at)))
            })
            .collect::<Result<Vec<_>>>()?;
        keys.reverse();
        Ok(keys)
    }

    /// Times a rule fired (or awaited approval) after `since`, newest first, at most `limit`.
    /// The daemon rebuilds cooldown, rate-limit and breaker state from this at startup.
    pub fn recent_fires(&self, rule: &str, since: Timestamp, limit: u32) -> Result<Vec<Timestamp>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT at FROM deliveries WHERE rule = ?1 AND outcome IN ('fired', 'awaiting_approval') AND at > ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let times = stmt
            .query_map(params![rule, since.as_millis(), limit], |r| r.get::<_, i64>(0))?
            .map(|t| t.map(Timestamp::from_millis))
            .collect::<rusqlite::Result<_>>()?;
        Ok(times)
    }
}
