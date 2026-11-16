//! A fake [`Backend`] the tests control, and an in-process MCP client.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pulse_contracts::catalog::{CATALOG, EventTypeInfo};
use pulse_contracts::rpc::{EventFilter, EventPage, EventQuery, EventsBatch, Order};
use pulse_contracts::{
    Event, EventId, EventType, NewEvent, SourceKind, SourceRef, SubscriptionId, Timestamp, Ulid,
};
use pulse_mcp::{AgentIdentity, AgentSubscription, Backend, BackendError, Clock, McpConfig};
use rmcp::model::{
    CallToolRequestParams, ClientRequest, CustomNotification, CustomRequest,
    NotificationMetaObject, ProgressNotificationParam, ProtocolVersion, ServerResult,
};
use rmcp::service::{ClientLifecycleMode, NotificationContext, RunningService};
use rmcp::{ClientHandler, RoleClient, serve_client_with_lifecycle};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

/// Agents, by token: `alice` may see `fs.*` under `/home/me/University` and `calendar.*`; `bob`
/// may see `git.*`.
pub struct Fake {
    log: Mutex<Vec<Event>>,
    subs: Mutex<Vec<LiveSub>>,
    now_ms: Arc<AtomicI64>,
    seq: AtomicU64,
    revoked: Mutex<Vec<String>>,
    /// Bumped after every `query_events`, so a test can wait until a read has happened.
    pub queries: watch::Sender<u64>,
    /// Bumped after every `resolve_agent`.
    pub resolves: watch::Sender<u64>,
}

struct LiveSub {
    id: SubscriptionId,
    agent: AgentIdentity,
    filter: EventFilter,
    tx: mpsc::Sender<EventsBatch>,
}

pub const ALICE_TOKEN: &str = "tok-alice";
pub const BOB_TOKEN: &str = "tok-bob";

pub fn alice() -> AgentIdentity {
    AgentIdentity { name: "alice".into() }
}

pub fn bob() -> AgentIdentity {
    AgentIdentity { name: "bob".into() }
}

impl Fake {
    pub fn new() -> Arc<Self> {
        Arc::new(Fake {
            log: Mutex::default(),
            subs: Mutex::default(),
            now_ms: Arc::new(AtomicI64::new(1_763_000_000_000)),
            seq: AtomicU64::new(1),
            revoked: Mutex::default(),
            queries: watch::channel(0).0,
            resolves: watch::channel(0).0,
        })
    }

    /// A config whose clock is this fake's.
    pub fn config(&self) -> McpConfig {
        let now = self.now_ms.clone();
        McpConfig {
            events_extension: true,
            clock: Clock::new(move || Timestamp::from_millis(now.load(Ordering::SeqCst))),
            ..McpConfig::default()
        }
    }

    /// Moves the wall clock on by a millisecond, so later events sort after "now" cursors.
    pub fn tick(&self) {
        self.now_ms.fetch_add(1, Ordering::SeqCst);
    }

    pub fn emit(&self, type_: &str, subject: Option<&str>, data: Value) -> Event {
        let kind = match type_.split('.').next() {
            Some("git") => SourceKind::Git,
            Some("calendar") => SourceKind::Calendar,
            Some("ext") => SourceKind::Ext,
            _ => SourceKind::Fs,
        };
        let ms = self.now_ms.load(Ordering::SeqCst);
        let id = EventId::from_ulid(Ulid::from_parts(
            u64::try_from(ms).unwrap(),
            u128::from(self.seq.fetch_add(1, Ordering::SeqCst)),
        ));
        let event = NewEvent {
            type_: EventType::new(type_).unwrap(),
            source: SourceRef::new(kind, "src"),
            subject: subject.map(str::to_owned),
            time: Timestamp::from_millis(ms),
            data,
            dedupe_key: None,
            coalesced: None,
            causation_run: None,
        }
        .into_event(id, None, None);
        self.log.lock().unwrap().push(event.clone());
        // Live fan-out drops batches when a subscriber is behind, like the daemon's bounded
        // channels. The log keeps everything.
        for s in self.subs.lock().unwrap().iter() {
            if visible(&s.agent, &event) && matches(&s.filter, &event) {
                let _ = s.tx.try_send(EventsBatch {
                    subscription: s.id,
                    events: vec![event.clone()],
                    dropped: 0,
                });
            }
        }
        event
    }

    pub fn file(&self, path: &str) -> Event {
        self.emit("fs.file.created", Some(path), json!({ "path": path }))
    }

    /// Live backend subscriptions whose receiver is still held.
    pub fn live(&self) -> usize {
        self.subs.lock().unwrap().iter().filter(|s| !s.tx.is_closed()).count()
    }

    /// Resolves when every live subscription of `agent` has been dropped by the server.
    pub async fn all_closed(&self, agent: &AgentIdentity) {
        let senders: Vec<_> = self
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.agent == *agent)
            .map(|s| s.tx.clone())
            .collect();
        for tx in senders {
            tx.closed().await;
        }
    }

    /// Resolves when every live subscription whose filter satisfies `f` has been dropped.
    pub async fn closed_where(&self, f: impl Fn(&EventFilter) -> bool) {
        let senders: Vec<_> = self
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| f(&s.filter))
            .map(|s| s.tx.clone())
            .collect();
        assert!(!senders.is_empty(), "no such subscription");
        for tx in senders {
            tx.closed().await;
        }
    }

    /// Takes `agent`'s access away and ends its live subscriptions, as the daemon would when an
    /// agent is deleted.
    pub fn revoke(&self, agent: &AgentIdentity) {
        self.revoked.lock().unwrap().push(agent.name.clone());
        self.subs.lock().unwrap().retain(|s| s.agent != *agent);
    }

    fn is_revoked(&self, agent: &AgentIdentity) -> bool {
        self.revoked.lock().unwrap().contains(&agent.name)
    }
}

fn grants(agent: &AgentIdentity) -> &'static [&'static str] {
    match agent.name.as_str() {
        "alice" => &["fs.*", "calendar.*"],
        "bob" => &["git.*"],
        _ => &[],
    }
}

fn type_matches(glob: &str, type_: &str) -> bool {
    match glob.strip_suffix('*') {
        Some(prefix) => type_.starts_with(prefix),
        None => glob == type_,
    }
}

fn visible(agent: &AgentIdentity, e: &Event) -> bool {
    let typed = grants(agent).iter().any(|g| type_matches(g, e.type_.as_str()));
    // alice's fs scope is path-qualified.
    let in_path = !e.type_.as_str().starts_with("fs.")
        || e.paths().iter().all(|p| p.starts_with("/home/me/University/"));
    typed && in_path
}

fn matches(f: &EventFilter, e: &Event) -> bool {
    (f.types.is_empty() || f.types.iter().any(|t| type_matches(t.as_str(), e.type_.as_str())))
        && f.path.as_deref().is_none_or(|p| e.paths().iter().any(|x| x.starts_with(p)))
        && f.subject.as_deref().is_none_or(|g| {
            let s = e.subject.as_deref().unwrap_or("");
            g.strip_suffix('*').map_or(s == g, |prefix| s.starts_with(prefix))
        })
        && f.text.as_deref().is_none_or(|t| {
            let t = t.to_lowercase();
            e.subject.as_deref().is_some_and(|s| s.to_lowercase().contains(&t))
                || e.data.to_string().to_lowercase().contains(&t)
        })
        && f.since.is_none_or(|s| e.time >= s)
        && f.until.is_none_or(|u| e.time < u)
}

impl Backend for Fake {
    async fn resolve_agent(&self, token: &str) -> Option<AgentIdentity> {
        self.resolves.send_modify(|n| *n += 1);
        match token {
            ALICE_TOKEN => Some(alice()),
            BOB_TOKEN => Some(bob()),
            _ => None,
        }
    }

    fn event_types(&self, agent: &AgentIdentity) -> Vec<EventTypeInfo> {
        CATALOG
            .iter()
            .filter(|c| grants(agent).iter().any(|g| type_matches(g, c.type_)))
            .map(|c| EventTypeInfo {
                type_: c.type_.to_owned(),
                description: c.description.to_owned(),
                subject: c.subject.to_owned(),
                path_fields: c.paths.iter().map(|p| p.describe()).collect(),
                example: (c.example)(),
            })
            .collect()
    }

    /// Paginates like the store (cursor exclusive, `limit + 1` probe), then removes what the
    /// agent may not see, so pages come back short exactly as the daemon's would.
    async fn query_events(
        &self,
        agent: &AgentIdentity,
        q: EventQuery,
    ) -> Result<EventPage, BackendError> {
        if self.is_revoked(agent) {
            return Err(BackendError::Unauthorized);
        }
        let limit = pulse_contracts::rpc::page_limit(q.limit) as usize;
        let log = self.log.lock().unwrap();
        let mut rows: Vec<&Event> = log
            .iter()
            .filter(|e| matches(&q.filter, e))
            .filter(|e| match (q.cursor, q.order) {
                (None, _) => true,
                (Some(c), Order::OldestFirst) => e.id > c,
                (Some(c), Order::NewestFirst) => e.id < c,
            })
            .collect();
        if q.order == Order::NewestFirst {
            rows.reverse();
        }
        rows.truncate(limit + 1);
        let next_cursor = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|e| e.id)
        } else {
            None
        };
        let events = rows.into_iter().filter(|e| visible(agent, e)).cloned().collect();
        drop(log);
        self.queries.send_modify(|n| *n += 1);
        Ok(EventPage { events, next_cursor })
    }

    async fn subscribe(
        &self,
        agent: &AgentIdentity,
        filter: EventFilter,
    ) -> Result<AgentSubscription, BackendError> {
        if self.is_revoked(agent) {
            return Err(BackendError::Unauthorized);
        }
        // Capacity 1: a subscriber that is not draining loses live batches.
        let (tx, rx) = mpsc::channel(1);
        let id = SubscriptionId::from_ulid(Ulid::from_parts(
            u64::try_from(self.now_ms.load(Ordering::SeqCst)).unwrap(),
            u128::from(self.seq.fetch_add(1, Ordering::SeqCst)),
        ));
        self.subs.lock().unwrap().push(LiveSub { id, agent: agent.clone(), filter, tx });
        Ok(AgentSubscription { id, events: rx })
    }
}

/// What the test client received besides responses.
#[derive(Debug)]
pub enum Note {
    Custom(CustomNotification),
    Progress(ProgressNotificationParam),
}

pub struct Recorder(pub mpsc::UnboundedSender<Note>);

impl ClientHandler for Recorder {
    /// rmcp hands a notification's `_meta` to the handler separately (for custom notifications,
    /// inside `ctx.extensions`); put it back where it was on the wire, so tests read params as
    /// sent.
    async fn on_custom_notification(
        &self,
        mut n: CustomNotification,
        ctx: NotificationContext<RoleClient>,
    ) {
        let meta = ctx
            .extensions
            .get::<NotificationMetaObject>()
            .map(|m| m.0.0.clone())
            .unwrap_or_else(|| ctx.meta.0.0.clone());
        if let Some(Value::Object(p)) = n.params.as_mut() {
            p.insert("_meta".into(), Value::Object(meta));
        }
        let _ = self.0.send(Note::Custom(n));
    }

    async fn on_progress(
        &self,
        p: ProgressNotificationParam,
        _ctx: NotificationContext<RoleClient>,
    ) {
        let _ = self.0.send(Note::Progress(p));
    }
}

pub type Client = RunningService<RoleClient, Recorder>;

/// An MCP client on protocol 2026-07-28 connected in-process to a server bound to `agent`.
pub async fn connect(
    fake: &Arc<Fake>,
    config: McpConfig,
    agent: AgentIdentity,
) -> (Client, mpsc::UnboundedReceiver<Note>) {
    connect_with(fake, config, agent, discover()).await
}

pub fn discover() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] }
}

pub async fn connect_with(
    fake: &Arc<Fake>,
    config: McpConfig,
    agent: AgentIdentity,
    lifecycle: ClientLifecycleMode,
) -> (Client, mpsc::UnboundedReceiver<Note>) {
    let (client_io, server_io) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server_io);
    tokio::spawn(pulse_mcp::serve_io(fake.clone(), config, agent, sr, sw));
    let (cr, cw) = tokio::io::split(client_io);
    let (tx, rx) = mpsc::unbounded_channel();
    let client = serve_client_with_lifecycle(Recorder(tx), (cr, cw), lifecycle).await.unwrap();
    (client, rx)
}

pub fn call(name: &'static str, args: Value) -> CallToolRequestParams {
    let mut p = CallToolRequestParams::new(name);
    p.arguments = args.as_object().cloned();
    p
}

/// Calls a tool and returns `(is_error, text)`.
pub async fn tool(client: &Client, name: &'static str, args: Value) -> (bool, String) {
    let r = client.call_tool(call(name, args)).await.unwrap();
    let text = r
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    (r.is_error.unwrap_or(false), text)
}

/// The summary line and the fenced events of a tool result.
pub fn split(text: &str) -> (Value, Vec<Value>) {
    let (summary, rest) = text.split_once('\n').unwrap_or((text, ""));
    let summary: Value = serde_json::from_str(summary).unwrap();
    let Some(start) = rest.find("\nPULSE-UNTRUSTED-DATA-") else {
        return (summary, Vec::new());
    };
    let block = &rest[start + 1..];
    let (fence, body) = block.split_once('\n').unwrap();
    let end = body.find(fence).unwrap();
    (summary, serde_json::from_str(&body[..end]).unwrap())
}

pub fn ids(events: &[Value]) -> Vec<String> {
    events.iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect()
}

/// Sends an extension request and returns its result or its error.
pub async fn custom(
    client: &Client,
    method: &'static str,
    params: Value,
) -> Result<Value, rmcp::model::ErrorData> {
    let request = ClientRequest::CustomRequest(CustomRequest::new(method, Some(params)));
    match client.peer().send_request(request).await {
        Ok(ServerResult::CustomResult(r)) => Ok(r.0),
        Ok(other) => panic!("unexpected result {other:?}"),
        Err(rmcp::ServiceError::McpError(e)) => Err(e),
        Err(e) => panic!("{e}"),
    }
}

/// The next custom notification, skipping progress.
pub async fn next_custom(rx: &mut mpsc::UnboundedReceiver<Note>) -> CustomNotification {
    loop {
        match rx.recv().await.expect("client closed") {
            Note::Custom(n) => return n,
            Note::Progress(_) => {}
        }
    }
}
