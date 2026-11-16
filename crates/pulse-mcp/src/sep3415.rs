//! The MCP Events extension, SEP-3415, as of PR modelcontextprotocol/modelcontextprotocol#3415
//! at commit `4165cc2e` (2026-11-14; voted Accepted 2026-11-11, unmerged). Requires protocol
//! revision 2026-07-28. This module is the only code that knows the extension's wire names.
//!
//! Implemented: capability advertisement, `events/list`, poll (`events/poll`) and push
//! (`events/stream`). Not implemented: webhook delivery (`events/subscribe` answers
//! `Unsupported`), `notifications/events/list_changed`. Why, and how to update this module when
//! the SEP changes: `docs/internals/0007-mcp-and-sep-3415.md`.
//!
//! Mapping onto Pulse:
//! - An event name is a Pulse event type (`fs.file.created`). `ext.*` is not listed (it is a
//!   namespace, not a name), but a concrete `ext.<source>.<name>` can be polled or streamed by
//!   an agent whose catalog includes `ext.*`.
//! - A cursor is the id (ULID) of the last event delivered, or a position derived from the
//!   clock for "from now". Both sort with event ids, so one comparison serves every case.
//! - Every read goes through [`Backend::query_events`] as the authenticated agent, so scopes
//!   are re-checked at delivery time, not only at subscribe time.
//! - The envelope is translated from CloudEvents in [`occurrence`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine;
use pulse_contracts::config::{PromptLimits, TypeGlob};
use pulse_contracts::rpc::EventFilter;
use pulse_contracts::{Event, EventId, Ulid};
use pulse_core::prompt::bounded_data_json;
use rmcp::RoleServer;
use rmcp::model::{
    CustomNotification, CustomRequest, CustomResult, ErrorCode, ErrorData, ExtensionCapabilities,
    MetaObject, NotificationMetaObject, ServerNotification,
};
use rmcp::service::RequestContext;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use crate::subscriptions::{head, read_after};
use crate::{AgentIdentity, Backend, BackendError, Shared};

pub(crate) const EXTENSION: &str = "io.modelcontextprotocol/events";
const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

// Error codes. The numbers are provisional in the SEP ("assume -32020 to -32022 are the only
// codes allocated when this SEP is accepted"); `HeaderMismatch` is core (-32020).
const NOT_FOUND: i32 = -32023;
const FORBIDDEN: i32 = -32024;
const RESOURCE_EXHAUSTED: i32 = -32025;
const UNSUPPORTED: i32 = -32026;

const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);
const MIN_HEARTBEAT: Duration = Duration::from_secs(5);
const MAX_HEARTBEAT: Duration = Duration::from_secs(60);
const NEXT_POLL_MS: u64 = 5_000;
const DEFAULT_POLL_EVENTS: u32 = 50;
const MAX_POLL_EVENTS: u32 = 200;
/// Events read per log page while streaming.
const STREAM_PAGE: u32 = 100;

pub(crate) fn capability() -> ExtensionCapabilities {
    // `listChanged` is not offered: rmcp cannot carry `eventsListChanged` on
    // `subscriptions/listen`, and SEP-3371 disputes extensions adding it there.
    let mut ext = ExtensionCapabilities::new();
    ext.insert(EXTENSION.to_owned(), Map::new());
    ext
}

pub(crate) fn handles(method: &str) -> bool {
    method.starts_with("events/")
}

/// Push streams per agent, so one agent cannot hold an unbounded number open.
#[derive(Debug, Default)]
pub(crate) struct StreamCounts(Arc<Mutex<HashMap<AgentIdentity, usize>>>);

struct StreamSlot(Arc<Mutex<HashMap<AgentIdentity, usize>>>, AgentIdentity);

impl StreamCounts {
    fn take(&self, agent: &AgentIdentity, max: usize) -> Option<StreamSlot> {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let n = map.entry(agent.clone()).or_default();
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(StreamSlot(self.0.clone(), agent.clone()))
    }
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = map.get_mut(&self.1) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&self.1);
            }
        }
    }
}

fn err(code: i32, message: &'static str, data: Value) -> ErrorData {
    ErrorData::new(ErrorCode(code), message, Some(data))
}

fn not_found_event(name: &str) -> ErrorData {
    err(NOT_FOUND, "NotFound", json!({ "kind": "event", "name": name }))
}

fn invalid_params(reason: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(reason.into(), None)
}

impl From<BackendError> for ErrorData {
    fn from(e: BackendError) -> Self {
        match e {
            BackendError::Unauthorized => {
                err(FORBIDDEN, "Forbidden", json!({ "reason": "not authorized" }))
            }
            BackendError::Invalid(m) => invalid_params(m),
            BackendError::Unavailable(m) => ErrorData::internal_error(m, None),
        }
    }
}

pub(crate) async fn handle<B: Backend>(
    shared: &Arc<Shared<B>>,
    agent: &AgentIdentity,
    request: CustomRequest,
    ctx: RequestContext<RoleServer>,
) -> Result<CustomResult, ErrorData> {
    let params = request.params.unwrap_or(Value::Null);
    let result = match request.method.as_str() {
        "events/list" => list(shared, agent),
        "events/poll" => {
            check_name_header(&ctx, &params)?;
            poll(shared, agent, parse(params)?).await?
        }
        "events/stream" => {
            check_name_header(&ctx, &params)?;
            stream(shared, agent, parse(params)?, &ctx).await?
        }
        "events/subscribe" | "events/unsubscribe" => {
            check_name_header(&ctx, &params)?;
            return Err(err(
                UNSUPPORTED,
                "Unsupported",
                json!({ "feature": "deliveryMode", "value": "webhook" }),
            ));
        }
        _ => return Err(ErrorData::new(ErrorCode::METHOD_NOT_FOUND, request.method, None)),
    };
    Ok(CustomResult::new(result))
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ErrorData> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| invalid_params(format!("invalid params: {e}")))
}

/// Whether a missing `Mcp-Name` is rejected. The SEP says MUST, and the conformance suite
/// grades it. rmcp's own client does not send the header for extension methods (and the
/// TypeScript SDK's reportedly does not either); if a real client is locked out by this, set it
/// to `false` and a mismatch is still rejected.
const REQUIRE_MCP_NAME: bool = true;

/// Streamable HTTP: `Mcp-Name` must be present and equal `params.name` (SEP-2243 encoding),
/// else `HeaderMismatch`, which rmcp answers with HTTP 400. stdio has no headers.
fn check_name_header(ctx: &RequestContext<RoleServer>, params: &Value) -> Result<(), ErrorData> {
    let Some(parts) = ctx.extensions.get::<http::request::Parts>() else { return Ok(()) };
    let mismatch = |what: &'static str| {
        Err(ErrorData::header_mismatch(what, Some(json!({ "header": "Mcp-Name" }))))
    };
    let Some(raw) = parts.headers.get("mcp-name") else {
        return if REQUIRE_MCP_NAME { mismatch("missing Mcp-Name header") } else { Ok(()) };
    };
    let decoded = raw.to_str().ok().and_then(decode_header_value);
    let expected = params.get("name").and_then(Value::as_str);
    if decoded.is_none() || decoded.as_deref() != expected {
        return mismatch("Mcp-Name header does not match params.name");
    }
    Ok(())
}

fn decode_header_value(raw: &str) -> Option<String> {
    match raw.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        Some(b64) => base64::engine::general_purpose::STANDARD
            .decode(b64)
            .ok()
            .and_then(|b| String::from_utf8(b).ok()),
        None => Some(raw.to_owned()),
    }
}

// ---- events/list ----

fn list<B: Backend>(shared: &Shared<B>, agent: &AgentIdentity) -> Value {
    let events: Vec<Value> = shared
        .backend
        .event_types(agent)
        .into_iter()
        .filter(|t| !t.type_.contains('*'))
        .map(|t| {
            json!({
                "name": t.type_,
                "description": t.description,
                "delivery": ["poll", "push"],
                "inputSchema": input_schema(),
                "payloadSchema": payload_schema(&t.example),
                "_meta": {
                    "pulse/subject": t.subject,
                    "pulse/pathFields": t.path_fields,
                    "pulse/example": t.example,
                },
            })
        })
        .collect();
    json!({ "resultType": "complete", "events": events })
}

/// Subscription arguments: Pulse's filters, the same for every type.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    #[serde(default)]
    sources: Vec<String>,
    path: Option<String>,
    subject: Option<String>,
    text: Option<String>,
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string", "description": "Only events carrying this absolute path or a path under it." },
            "subject": { "type": "string", "description": "A glob over the event subject." },
            "text": { "type": "string", "description": "Case-insensitive text in the subject or data." },
            "sources": { "type": "array", "items": { "type": "string" }, "description": "Pulse source names or pulse:// URIs." },
        },
    })
}

/// The top-level shape of a type's `data`, from its catalog example. Deliberately loose: only
/// fields and JSON types, all optional, so later additions stay additive.
fn payload_schema(example: &Value) -> Value {
    let props: Map<String, Value> = example
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), json!({ "type": json_type(v) }))).collect())
        .unwrap_or_default();
    json!({ "type": "object", "properties": props })
}

fn json_type(v: &Value) -> Value {
    match v {
        Value::Null => json!(["string", "number", "boolean", "array", "object", "null"]),
        Value::Bool(_) => json!("boolean"),
        Value::Number(n) if n.is_i64() || n.is_u64() => json!(["integer", "null"]),
        Value::Number(_) => json!(["number", "null"]),
        Value::String(_) => json!(["string", "null"]),
        Value::Array(_) => json!("array"),
        Value::Object(_) => json!("object"),
    }
}

// ---- shared by poll and stream ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PollParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    max_age_ms: Option<u64>,
    #[serde(default)]
    max_events: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    max_age_ms: Option<u64>,
    #[serde(default)]
    heartbeat_ms: Option<u64>,
}

/// A validated subscription: what to read, and where to start (`None`: from now).
struct Subscription {
    filter: EventFilter,
    start: Option<EventId>,
    truncated: bool,
}

fn resolve<B: Backend>(
    shared: &Shared<B>,
    agent: &AgentIdentity,
    name: &str,
    arguments: Option<Value>,
    cursor: Option<String>,
    max_age_ms: Option<u64>,
) -> Result<Subscription, ErrorData> {
    if pulse_contracts::catalog::entry(name).is_none() || name.contains('*') {
        return Err(not_found_event(name));
    }
    let visible = shared.backend.event_types(agent);
    let covered = visible.iter().any(|t| {
        t.type_ == name
            || (t.type_ == pulse_contracts::catalog::EXTERNAL_PATTERN && name.starts_with("ext."))
    });
    if !covered {
        return Err(err(
            FORBIDDEN,
            "Forbidden",
            json!({ "reason": format!("agent {:?} may not receive {name}", agent.name) }),
        ));
    }
    let types = vec![name.parse::<TypeGlob>().map_err(|_| not_found_event(name))?];
    let args: Arguments = match arguments {
        None | Some(Value::Null) => Arguments::default(),
        Some(v) => serde_json::from_value(v)
            .map_err(|e| invalid_params(format!("arguments do not match inputSchema: {e}")))?,
    };
    if args.path.as_deref().is_some_and(|p| !p.starts_with('/')) {
        return Err(invalid_params("arguments.path must be an absolute path"));
    }
    let filter = EventFilter {
        types,
        sources: args.sources,
        subject: args.subject,
        path: args.path,
        text: args.text,
        ..EventFilter::default()
    };
    let clock = &shared.config.clock;
    // `null` and absent both mean "from now", and `maxAgeMs` is then ignored.
    let Some(cursor) = cursor else {
        return Ok(Subscription { filter, start: None, truncated: false });
    };
    let cursor: EventId = cursor.parse().map_err(|_| invalid_params("unrecognized cursor"))?;
    let floor = max_age_ms.map(|age| {
        let now = u64::try_from(clock.now().as_millis()).unwrap_or(0);
        EventId::from_ulid(Ulid::from_parts(now.saturating_sub(age), 0))
    });
    Ok(match floor {
        Some(floor) if floor > cursor => {
            Subscription { filter, start: Some(floor), truncated: true }
        }
        _ => Subscription { filter, start: Some(cursor), truncated: false },
    })
}

/// The SEP's `EventOccurrence`, from a Pulse CloudEvent.
///
/// | SEP          | Pulse                                                     |
/// |--------------|-----------------------------------------------------------|
/// | `eventId`    | `id` (ULID; stable across poll, push and replay of the log) |
/// | `name`       | `type`                                                    |
/// | `timestamp`  | `time` (RFC 3339, UTC, milliseconds)                      |
/// | `data`       | `data`, size-bounded; a non-object is wrapped as `{"value": …}` |
/// | `_meta`      | `pulse/source`, `pulse/subject`, `pulse/causation`, `pulse/replayOf`, `pulse/shortened` |
///
/// `cursor` is added by the push path; poll carries it at result level.
fn occurrence(event: &Event, field_max_bytes: u32) -> Value {
    let limits = PromptLimits {
        inline_max_chars: 200,
        field_max_bytes,
        data_max_bytes: field_max_bytes.saturating_mul(16),
    };
    let (bounded, shortened) = bounded_data_json(&event.data, &limits);
    let data = match serde_json::from_str::<Value>(&bounded) {
        Ok(Value::Object(o)) => Value::Object(o),
        Ok(other) => json!({ "value": other }),
        Err(_) => json!({}),
    };
    let mut meta = Map::new();
    meta.insert("pulse/source".into(), json!(event.source));
    if let Some(subject) = &event.subject {
        meta.insert("pulse/subject".into(), json!(subject));
    }
    if let Some(c) = event.causation {
        meta.insert("pulse/causation".into(), json!(c));
    }
    if let Some(r) = event.replay_of {
        meta.insert("pulse/replayOf".into(), json!(r));
    }
    if shortened {
        meta.insert("pulse/shortened".into(), json!(true));
    }
    json!({
        "eventId": event.id,
        "name": event.type_,
        "timestamp": event.time,
        "data": data,
        "_meta": meta,
    })
}

// ---- events/poll ----

async fn poll<B: Backend>(
    shared: &Shared<B>,
    agent: &AgentIdentity,
    p: PollParams,
) -> Result<Value, ErrorData> {
    let sub = resolve(shared, agent, &p.name, p.arguments, p.cursor, p.max_age_ms)?;
    let field_max = shared.config.limits.field_max_bytes;
    let Some(start) = sub.start else {
        let now = head(shared.backend.as_ref(), agent, &shared.config.clock).await?;
        return Ok(json!({
            "resultType": "complete", "events": [], "cursor": now,
            "truncated": false, "hasMore": false, "nextPollMs": NEXT_POLL_MS,
        }));
    };
    let max = p.max_events.unwrap_or(DEFAULT_POLL_EVENTS).clamp(1, MAX_POLL_EVENTS);
    let read = read_after(shared.backend.as_ref(), agent, &sub.filter, start, max).await?;
    let events: Vec<Value> = read.events.iter().map(|e| occurrence(e, field_max)).collect();
    Ok(json!({
        "resultType": "complete",
        "events": events,
        "cursor": read.cursor,
        "truncated": sub.truncated,
        "hasMore": read.has_more,
        "nextPollMs": NEXT_POLL_MS,
    }))
}

// ---- events/stream ----

struct Stream<'a> {
    ctx: &'a RequestContext<RoleServer>,
    meta: Value,
    last_sent: Instant,
}

impl Stream<'_> {
    /// Sends one `notifications/events/*`, tagged with the stream's subscription id. `Err` when
    /// the client is gone.
    async fn send(&mut self, method: &'static str, mut params: Value) -> Result<(), ErrorData> {
        // rmcp writes `_meta` from the notification's meta, not from `params`: move any
        // occurrence `_meta` there, next to the subscription id.
        let mut meta = match params.as_object_mut().and_then(|p| p.remove("_meta")) {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        meta.insert(SUBSCRIPTION_ID.into(), self.meta.clone());
        let mut notification = CustomNotification::new(method, Some(params));
        notification.extensions.insert(NotificationMetaObject(MetaObject(meta)));
        self.ctx
            .peer
            .send_notification(ServerNotification::CustomNotification(notification))
            .await
            .map_err(|e| ErrorData::internal_error(format!("stream closed: {e}"), None))?;
        self.last_sent = Instant::now();
        Ok(())
    }

    fn done(&self) -> Value {
        json!({ "resultType": "complete", "_meta": { SUBSCRIPTION_ID: self.meta } })
    }
}

async fn stream<B: Backend>(
    shared: &Shared<B>,
    agent: &AgentIdentity,
    p: StreamParams,
    ctx: &RequestContext<RoleServer>,
) -> Result<Value, ErrorData> {
    let sub = resolve(shared, agent, &p.name, p.arguments, p.cursor, p.max_age_ms)?;
    let max_streams = shared.config.limits.streams_per_agent;
    let _slot = shared.streams.take(agent, max_streams).ok_or_else(|| {
        err(
            RESOURCE_EXHAUSTED,
            "ResourceExhausted",
            json!({ "limit": "streams", "max": max_streams }),
        )
    })?;
    // Subscribed before `active` is sent and before the first read, so an event committed in
    // between wakes the loop instead of being missed.
    let mut live = shared.backend.subscribe(agent, sub.filter.clone()).await?;
    let heartbeat = p
        .heartbeat_ms
        .map_or(DEFAULT_HEARTBEAT, Duration::from_millis)
        .clamp(MIN_HEARTBEAT, MAX_HEARTBEAT);
    let field_max = shared.config.limits.field_max_bytes;
    let mut out = Stream {
        ctx,
        meta: serde_json::to_value(&ctx.id).unwrap_or(Value::Null),
        last_sent: Instant::now(),
    };
    let mut cursor = match sub.start {
        Some(c) => c,
        None => head(shared.backend.as_ref(), agent, &shared.config.clock).await?,
    };
    out.send(
        "notifications/events/active",
        json!({
            "cursor": cursor,
            "truncated": sub.truncated,
            "heartbeatMs": u64::try_from(heartbeat.as_millis()).unwrap_or(u64::MAX),
        }),
    )
    .await?;
    // A replay from a cursor, or anything that arrived since "now" was taken.
    let mut pending = true;
    loop {
        while pending {
            match read_after(shared.backend.as_ref(), agent, &sub.filter, cursor, STREAM_PAGE).await
            {
                Ok(read) => {
                    for event in &read.events {
                        let mut occ = occurrence(event, field_max);
                        occ["cursor"] = json!(event.id);
                        out.send("notifications/events/event", occ).await?;
                    }
                    cursor = read.cursor;
                    pending = read.has_more;
                }
                Err(BackendError::Unauthorized) => {
                    out.send(
                        "notifications/events/terminated",
                        json!({ "error": { "code": FORBIDDEN, "message": "Forbidden", "data": { "reason": "access revoked" } } }),
                    )
                    .await?;
                    return Ok(out.done());
                }
                Err(e) => {
                    // Recoverable: the stream stays open and the next wakeup reads again.
                    out.send(
                        "notifications/events/error",
                        json!({ "error": { "code": -32603, "message": "UpstreamError", "data": { "reason": e.to_string() } } }),
                    )
                    .await?;
                    pending = false;
                }
            }
        }
        tokio::select! {
            () = ctx.ct.cancelled() => return Ok(out.done()),
            () = shared.config.shutdown.cancelled() => return Ok(out.done()),
            woken = live.wake.recv() => match woken {
                Some(_) => pending = true,
                // The backend ended the subscription: the agent was removed or lost access.
                None => {
                    out.send(
                        "notifications/events/terminated",
                        json!({ "error": { "code": FORBIDDEN, "message": "Forbidden", "data": { "reason": "subscription ended by Pulse" } } }),
                    )
                    .await?;
                    return Ok(out.done());
                }
            },
            () = tokio::time::sleep_until(out.last_sent + heartbeat) => {
                out.send("notifications/events/heartbeat", json!({ "cursor": cursor })).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pulse_contracts::{
        Causation, EventType, NewEvent, RunId, SourceKind, SourceRef, Timestamp,
    };

    use super::*;

    fn event(type_: &str, subject: Option<&str>, data: Value) -> Event {
        NewEvent {
            type_: EventType::new(type_).unwrap(),
            source: SourceRef::new(SourceKind::Fs, "university"),
            subject: subject.map(str::to_owned),
            time: Timestamp::from_millis(1_763_285_667_123),
            data,
            dedupe_key: None,
            coalesced: Some(3),
            causation_run: None,
        }
        .into_event(EventId::from_ulid(Ulid::from_parts(1_763_285_667_123, 7)), None, None)
    }

    #[test]
    fn envelope_translation() {
        let id = Ulid::from_parts(1_763_285_667_123, 7).to_string();
        let ts = Timestamp::from_millis(1_763_285_667_123).to_string();
        let mut caused = event("git.commit.created", Some("/r"), json!({ "sha": "abc" }));
        caused.causation = Some(Causation::Inferred);
        caused.causation_run = Some(RunId::from_ulid(Ulid::from_parts(1, 1)));
        let cases: Vec<(Event, Value)> = vec![
            (
                event(
                    "fs.file.created",
                    Some("/home/me/U/a.pdf"),
                    json!({ "path": "/home/me/U/a.pdf" }),
                ),
                json!({
                    "eventId": id, "name": "fs.file.created", "timestamp": ts,
                    "data": { "path": "/home/me/U/a.pdf" },
                    "_meta": { "pulse/source": "pulse://fs/university", "pulse/subject": "/home/me/U/a.pdf" },
                }),
            ),
            (
                // Non-object data is wrapped; no subject means no subject key.
                event("ext.ci.build", None, json!("failed")),
                json!({
                    "eventId": id, "name": "ext.ci.build", "timestamp": ts,
                    "data": { "value": "failed" },
                    "_meta": { "pulse/source": "pulse://fs/university" },
                }),
            ),
            (
                caused,
                json!({
                    "eventId": id, "name": "git.commit.created", "timestamp": ts,
                    "data": { "sha": "abc" },
                    "_meta": { "pulse/source": "pulse://fs/university", "pulse/subject": "/r", "pulse/causation": "inferred" },
                }),
            ),
        ];
        for (event, want) in cases {
            assert_eq!(occurrence(&event, 1024), want);
        }
        let long =
            occurrence(&event("fs.file.created", None, json!({ "x": "y".repeat(5000) })), 1024);
        assert_eq!(long["_meta"]["pulse/shortened"], true);
        assert!(long["data"]["x"].as_str().unwrap().len() <= 1024);
    }

    #[test]
    fn header_values_decode() {
        assert_eq!(decode_header_value("fs.file.created").as_deref(), Some("fs.file.created"));
        assert_eq!(decode_header_value("=?base64?w6k=?=").as_deref(), Some("é"));
        assert_eq!(decode_header_value("=?base64?!!?="), None);
    }

    #[test]
    fn payload_schema_is_loose() {
        let s = payload_schema(&json!({ "path": "/a", "size": 3, "gone": null }));
        assert_eq!(s["properties"]["path"]["type"], json!(["string", "null"]));
        assert_eq!(s["properties"]["size"]["type"], json!(["integer", "null"]));
        assert!(s.get("required").is_none());
    }
}
