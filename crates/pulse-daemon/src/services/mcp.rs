//! What an MCP client may ask, as the agent its token names. The `/mcp` transport
//! (`transport::mcp`) maps these onto `pulse_mcp::Backend`; this is where the rules are.
//!
//! - **Identity**: a token is an agent's only if it verifies (`auth::verify_agent_token`,
//!   constant time) and the agent is in the configuration in effect now. That is the one
//!   definition; nothing on `/mcp` is served without it.
//! - **Scope before paging**: history is filtered by the agent's `allow` list while it is
//!   read, so a page and its cursor never refer to an event the agent cannot see. Filtering
//!   after paging would let a filter (text, subject, path) probe out-of-scope events: an empty
//!   page with a cursor says "something matching exists, here". The exception is the scan
//!   budget: after [`SCAN_BUDGET`] matching rows the agent cannot see, the page ends with a
//!   cursor at the last row examined, so a reader is never stuck. That reveals only that at
//!   least that many hidden events match the filter.
//! - **Subscriptions are wakeups**: the MCP server reads the log by cursor itself, so a live
//!   subscription only says "something you can see, matching your filter, was committed". It
//!   ends when the agent is removed or can no longer receive any type.

use pulse_contracts::catalog::{EventTypeInfo, event_types};
use pulse_contracts::rpc::{EventFilter, EventPage, EventQuery, page_limit};
use pulse_contracts::{Event, SubscriptionId};
use pulse_core::filter::CompiledFilter;
use tokio::sync::{broadcast, mpsc};

use super::Services;
use crate::config::Snapshot;
use crate::error::{ServiceError, ServiceResult};

/// Matching rows an agent cannot see that one page reads past before it returns.
pub(crate) const SCAN_BUDGET: usize = 4096;
/// Rows read from the store at a time while filling a scoped page.
const SCAN_CHUNK: u32 = 256;

fn unknown_agent(agent: &str) -> ServiceError {
    ServiceError::not_found("agent", agent)
}

/// The catalog entries `agent` could ever receive, or `None` if it is not configured.
fn catalog_for(snapshot: &Snapshot, agent: &str) -> Option<Vec<EventTypeInfo>> {
    snapshot.config.agent(agent)?;
    Some(
        event_types()
            .into_iter()
            .filter(|t| {
                snapshot.routing.agent_may_receive(agent, &t.type_, !t.path_fields.is_empty())
            })
            .collect(),
    )
}

/// Whether `agent` still exists and can receive something.
fn has_access(snapshot: &Snapshot, agent: &str) -> bool {
    catalog_for(snapshot, agent).is_some_and(|c| !c.is_empty())
}

impl Services {
    /// The agent an MCP bearer token belongs to.
    pub fn mcp_agent(&self, token: &str) -> Option<String> {
        let agent = crate::auth::verify_agent_token(&self.shared.secret, token)?;
        self.shared.snapshot().config.agent(agent)?;
        Some(agent.to_owned())
    }

    /// The event types `agent` may be told about.
    pub fn mcp_event_types(&self, agent: &str) -> Vec<EventTypeInfo> {
        catalog_for(&self.shared.snapshot(), agent).unwrap_or_default()
    }

    /// History as `agent` sees it. A page holds up to `limit` events the agent may see, oldest
    /// or newest first; `next_cursor` continues after the last one (or after the scan budget).
    pub async fn mcp_query_events(
        &self,
        agent: &str,
        query: EventQuery,
    ) -> ServiceResult<EventPage> {
        let snapshot = self.shared.snapshot();
        if snapshot.config.agent(agent).is_none() {
            return Err(unknown_agent(agent));
        }
        let agent = agent.to_owned();
        let page = self
            .shared
            .store
            .read(move |q| {
                scoped_page(
                    &query,
                    SCAN_BUDGET,
                    |query| q.query().query_events(query),
                    |e| snapshot.routing.agent_permits(&agent, e),
                )
            })
            .await?;
        Ok(page)
    }

    /// A wakeup for each committed batch that holds an event matching `filter` that `agent`
    /// may see (checked against the configuration at delivery). At most one is pending: the
    /// reader catches up from the log. The channel closes when the agent is removed or loses
    /// access to every type, when the receiver is dropped, or at shutdown.
    pub(crate) fn mcp_wakeups(
        &self,
        agent: &str,
        filter: EventFilter,
    ) -> ServiceResult<(SubscriptionId, mpsc::Receiver<()>)> {
        if !has_access(&self.shared.snapshot(), agent) {
            return Err(unknown_agent(agent));
        }
        let compiled = CompiledFilter::compile(&filter)
            .map_err(|e| ServiceError::Invalid(format!("invalid filter: {e}")))?;
        let (tx, rx) = mpsc::channel(1);
        let mut events = self.shared.hub.subscribe_events();
        let mut config = self.shared.config_changes();
        let shared = self.shared.clone();
        let agent = agent.to_owned();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shared.shutdown.cancelled() => return,
                    () = tx.closed() => return,
                    changed = config.changed() => {
                        if changed.is_err() || !has_access(&shared.snapshot(), &agent) {
                            return;
                        }
                    }
                    batch = events.recv() => {
                        let wake = match batch {
                            Ok(batch) => {
                                let snapshot = shared.snapshot();
                                batch.iter().any(|e| {
                                    compiled.matches(e) && snapshot.routing.agent_permits(&agent, e)
                                })
                            }
                            // Batches were missed; let the reader look.
                            Err(broadcast::error::RecvError::Lagged(_)) => true,
                            Err(broadcast::error::RecvError::Closed) => return,
                        };
                        if wake {
                            // Full means a wakeup is already pending: nothing to add.
                            let _ = tx.try_send(());
                        }
                    }
                }
            }
        });
        Ok((self.shared.ids.subscription(), rx))
    }
}

/// Fills a page of `query.limit` events that pass `permit`, reading `SCAN_CHUNK` rows at a
/// time through `read` (the store's unscoped query), and stopping after `budget` rows that do
/// not pass.
fn scoped_page<E>(
    query: &EventQuery,
    budget: usize,
    mut read: impl FnMut(&EventQuery) -> Result<EventPage, E>,
    permit: impl Fn(&Event) -> bool,
) -> Result<EventPage, E> {
    let limit = page_limit(query.limit) as usize;
    let mut events = Vec::new();
    let mut cursor = query.cursor;
    let mut hidden = 0;
    loop {
        let chunk = EventQuery { cursor, limit: Some(SCAN_CHUNK), ..query.clone() };
        let page = read(&chunk)?;
        for e in page.events {
            cursor = Some(e.id);
            if !permit(&e) {
                hidden += 1;
                if hidden >= budget {
                    return Ok(EventPage { events, next_cursor: cursor });
                }
                continue;
            }
            if events.len() == limit {
                // One more visible event exists: continue after the last one returned.
                let next_cursor = events.last().map(|e: &Event| e.id);
                return Ok(EventPage { events, next_cursor });
            }
            events.push(e);
        }
        if page.next_cursor.is_none() {
            return Ok(EventPage { events, next_cursor: None });
        }
    }
}

#[cfg(test)]
mod tests {
    use pulse_contracts::rpc::Order;
    use pulse_contracts::{EventId, EventType, NewEvent, SourceKind, SourceRef, Timestamp, Ulid};

    use super::*;

    fn log(n: u64) -> Vec<Event> {
        (1..=n)
            .map(|i| {
                NewEvent {
                    type_: EventType::new("ext.t.x").unwrap(),
                    source: SourceRef::new(SourceKind::Ext, "t"),
                    subject: Some(i.to_string()),
                    time: Timestamp::from_millis(i as i64),
                    data: serde_json::json!({}),
                    dedupe_key: None,
                    coalesced: None,
                    causation_run: None,
                }
                .into_event(EventId::from_ulid(Ulid::from_parts(i, 0)), None, None)
            })
            .collect()
    }

    /// The store's paging over `log`: cursor exclusive, `limit + 1` probe.
    fn store(log: &[Event]) -> impl FnMut(&EventQuery) -> Result<EventPage, ()> + '_ {
        move |q| {
            let limit = page_limit(q.limit) as usize;
            let mut rows: Vec<Event> = log
                .iter()
                .filter(|e| match (q.cursor, q.order) {
                    (None, _) => true,
                    (Some(c), Order::OldestFirst) => e.id > c,
                    (Some(c), Order::NewestFirst) => e.id < c,
                })
                .cloned()
                .collect();
            if q.order == Order::NewestFirst {
                rows.reverse();
            }
            let next_cursor = (rows.len() > limit).then(|| rows[limit - 1].id);
            rows.truncate(limit);
            Ok(EventPage { events: rows, next_cursor })
        }
    }

    fn subjects(p: &EventPage) -> Vec<u64> {
        p.events.iter().map(|e| e.subject.as_deref().unwrap().parse().unwrap()).collect()
    }

    #[test]
    fn pages_hold_only_permitted_events_and_cursors_always_advance() {
        let log = log(2000);
        // Visible: multiples of 700 (700, 1400) and everything above 1990.
        let permit = |e: &Event| {
            let n: u64 = e.subject.as_deref().unwrap().parse().unwrap();
            n % 700 == 0 || n > 1990
        };
        let q = |cursor, limit, order| EventQuery {
            cursor,
            limit: Some(limit),
            order,
            ..EventQuery::default()
        };
        let first =
            scoped_page(&q(None, 2, Order::OldestFirst), 10_000, store(&log), permit).unwrap();
        assert_eq!(subjects(&first), [700, 1400]);
        // The cursor is the last visible event, never a hidden row.
        assert_eq!(first.next_cursor, Some(first.events[1].id));
        let rest =
            scoped_page(&q(first.next_cursor, 50, Order::OldestFirst), 10_000, store(&log), permit)
                .unwrap();
        assert_eq!(subjects(&rest), (1991..=2000).collect::<Vec<_>>());
        assert_eq!(rest.next_cursor, None);

        let newest =
            scoped_page(&q(None, 1, Order::NewestFirst), 10_000, store(&log), permit).unwrap();
        assert_eq!(subjects(&newest), [2000]);

        // With a small budget, a run of hidden rows ends the page early with a cursor past
        // them, so a reader advances instead of looping.
        let cut = scoped_page(&q(None, 5, Order::OldestFirst), 300, store(&log), permit).unwrap();
        assert!(cut.events.is_empty());
        assert_eq!(cut.next_cursor, Some(log[299].id));
        let mut cursor = cut.next_cursor;
        let mut seen = Vec::new();
        while let Some(c) = cursor {
            let p =
                scoped_page(&q(Some(c), 5, Order::OldestFirst), 300, store(&log), permit).unwrap();
            seen.extend(subjects(&p));
            cursor = p.next_cursor;
        }
        assert_eq!(seen, [700, 1400].into_iter().chain(1991..=2000).collect::<Vec<_>>());
    }
}
