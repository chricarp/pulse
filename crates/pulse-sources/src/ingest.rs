//! Parsing of external events for `POST /ingest` and `pulse ingest`: a JSON object, a JSON
//! array, or JSON lines. The result is typed `IngestEvent`s; the engine forces their type into
//! `ext.<source>.` and applies dedupe.
//!
//! Two shapes are accepted per event:
//! - **minimal**: `type`, and optionally `subject`, `time` (RFC 3339), `data`, `dedupe_key`,
//!   `causation_run`. Unknown fields are errors (they are usually typos).
//! - **CloudEvents 1.0** (recognised by `specversion`): `type`, `subject`, `time`, `data`;
//!   `id` becomes the dedupe key unless `pulsededupekey` is given; `pulsecausationrun` is the
//!   causing run; `source` is ignored (Pulse names the source); other extension attributes are
//!   ignored. `data_base64` is refused, and so are the `pulse*` attributes Pulse sets itself.
//!
//! **Namespace.** An external event can never have a built-in type. `type` is either a suffix
//! (`build.failed`, which becomes `ext.<source>.build.failed`) or already `ext.<…>`; a suffix
//! whose first segment is a built-in namespace (`fs.file.created`) is refused rather than
//! silently renamed, so a sender learns it cannot impersonate a built-in event.
//!
//! **Limits.** The body, the number of events, and each field are bounded. A batch is accepted
//! whole or refused whole; a refusal lists every bad item (up to [`MAX_REPORTED`]) with its index,
//! line and field.

use std::fmt;

use pulse_contracts::Timestamp;
use pulse_contracts::ids::RunId;
use pulse_contracts::rpc::IngestEvent;
use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unsupported content type {0:?}: send application/json or application/x-ndjson")]
    ContentType(String),
    #[error("line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("body is {size} bytes; the limit is {limit}")]
    TooLarge { size: usize, limit: usize },
    #[error("{count} events in one request; the limit is {limit}")]
    TooMany { count: usize, limit: usize },
    #[error("no events in the request")]
    Empty,
    #[error("{} invalid event(s): {}", .0.len(), .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Invalid(Vec<ItemError>),
}

/// What is wrong with one event of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemError {
    /// 0-based position in the batch.
    pub index: usize,
    /// 1-based line, for JSON lines.
    pub line: Option<usize>,
    pub field: Option<String>,
    pub message: String,
}

impl fmt::Display for ItemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event {}", self.index)?;
        if let Some(l) = self.line {
            write!(f, " (line {l})")?;
        }
        if let Some(field) = &self.field {
            write!(f, ", {field}")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// Limits for one ingest request.
#[derive(Clone, Copy, Debug)]
pub struct IngestLimits {
    pub max_body_bytes: usize,
    pub max_events: usize,
}

impl Default for IngestLimits {
    fn default() -> Self {
        IngestLimits { max_body_bytes: 1024 * 1024, max_events: 1000 }
    }
}

/// Per-field limits.
pub const MAX_TYPE_BYTES: usize = 128;
pub const MAX_SUBJECT_BYTES: usize = 1024;
pub const MAX_DEDUPE_KEY_BYTES: usize = 256;
/// `data`, serialized.
pub const MAX_DATA_BYTES: usize = 64 * 1024;
/// Item errors reported for one request.
pub const MAX_REPORTED: usize = 100;

/// Namespaces that belong to built-in sources.
const RESERVED: &[&str] = &["fs", "git", "process", "schedule", "calendar", "pulse"];
/// Attributes Pulse sets on events; senders may not.
const PULSE_SET: &[&str] =
    &["pulsecausation", "pulsechaindepth", "pulsereplayof", "pulsecoalesced"];

pub fn parse(
    content_type: Option<&str>,
    body: &[u8],
    limits: &IngestLimits,
) -> Result<Vec<IngestEvent>, IngestError> {
    if body.len() > limits.max_body_bytes {
        return Err(IngestError::TooLarge { size: body.len(), limit: limits.max_body_bytes });
    }
    let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let format = match content_type
        .map(|c| c.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
    {
        None => Format::Sniff,
        Some(c) => match c.as_str() {
            "" | "application/json" | "application/cloudevents+json" | "text/json" => Format::Sniff,
            "application/cloudevents-batch+json" => Format::Array,
            "application/x-ndjson"
            | "application/ndjson"
            | "application/jsonl"
            | "application/x-jsonlines"
            | "application/jsonlines" => Format::Lines,
            _ => return Err(IngestError::ContentType(c)),
        },
    };
    // (value, line) per item.
    let items: Vec<(Value, Option<usize>)> = match format {
        Format::Lines => lines(body)?,
        Format::Array | Format::Sniff => {
            let mut stream = serde_json::Deserializer::from_slice(body).into_iter::<Value>();
            let first = match stream.next() {
                None => return Err(IngestError::Empty),
                Some(Err(e)) => {
                    return Err(IngestError::Parse { line: e.line(), message: e.to_string() });
                }
                Some(Ok(v)) => v,
            };
            let more = body[stream.byte_offset()..].iter().any(|b| !b.is_ascii_whitespace());
            match (first, more, format) {
                (Value::Array(items), false, _) => items.into_iter().map(|v| (v, None)).collect(),
                (_, _, Format::Array) => {
                    return Err(IngestError::Parse {
                        line: 1,
                        message: "expected one JSON array of events".into(),
                    });
                }
                // Several values: JSON lines sent without saying so.
                (_, true, _) => lines(body)?,
                (v, false, _) => vec![(v, None)],
            }
        }
    };
    if items.is_empty() {
        return Err(IngestError::Empty);
    }
    if items.len() > limits.max_events {
        return Err(IngestError::TooMany { count: items.len(), limit: limits.max_events });
    }
    let mut events = Vec::with_capacity(items.len());
    let mut errors = Vec::new();
    for (index, (value, line)) in items.into_iter().enumerate() {
        match event(value) {
            Ok(e) => events.push(e),
            Err((field, message)) => {
                if errors.len() < MAX_REPORTED {
                    errors.push(ItemError { index, line, field, message });
                }
            }
        }
    }
    if errors.is_empty() { Ok(events) } else { Err(IngestError::Invalid(errors)) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Sniff,
    Array,
    Lines,
}

fn lines(body: &[u8]) -> Result<Vec<(Value, Option<usize>)>, IngestError> {
    let mut out = Vec::new();
    for (i, line) in body.split(|&b| b == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let v = serde_json::from_slice(line)
            .map_err(|e| IngestError::Parse { line: i + 1, message: e.to_string() })?;
        out.push((v, Some(i + 1)));
    }
    Ok(out)
}

type FieldError = (Option<String>, String);

fn err(field: &str, message: impl Into<String>) -> FieldError {
    (Some(field.to_owned()), message.into())
}

fn event(value: Value) -> Result<IngestEvent, FieldError> {
    let Value::Object(mut obj) = value else {
        return Err((None, "expected a JSON object".into()));
    };
    let cloud = obj.contains_key("specversion");
    if cloud {
        match obj.get("specversion") {
            Some(Value::String(v)) if v == "1.0" => {}
            _ => return Err(err("specversion", "only CloudEvents 1.0 is supported")),
        }
        if obj.contains_key("data_base64") {
            return Err(err("data_base64", "binary data is not supported; send JSON in `data`"));
        }
        if let Some(k) = PULSE_SET.iter().find(|k| obj.contains_key(**k)) {
            return Err(err(k, "is set by Pulse; remove it"));
        }
    } else {
        let allowed = ["type", "subject", "time", "data", "dedupe_key", "causation_run"];
        let unknown: Vec<&str> =
            obj.keys().map(String::as_str).filter(|k| !allowed.contains(k)).collect();
        if !unknown.is_empty() {
            return Err((
                None,
                format!(
                    "unknown field(s) {}; expected type, subject, time, data, dedupe_key, causation_run (or a CloudEvent with specversion)",
                    unknown
                        .iter()
                        .map(|k| format!("{:?}", bounded(k, 64)))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    let type_ = match obj.remove("type") {
        Some(Value::String(t)) => t,
        Some(_) => return Err(err("type", "must be a string")),
        None => return Err(err("type", "is required")),
    };
    check_type(&type_).map_err(|m| err("type", m))?;
    let subject = optional_string(&mut obj, "subject", MAX_SUBJECT_BYTES)?;
    let time = match optional_string(&mut obj, "time", 64)? {
        Some(t) => Some(Timestamp::parse_rfc3339(&t).map_err(|e| err("time", e.to_string()))?),
        None => None,
    };
    let data = obj.remove("data").unwrap_or(Value::Null);
    let size = serde_json::to_vec(&data).map_or(usize::MAX, |v| v.len());
    if size > MAX_DATA_BYTES {
        return Err(err(
            "data",
            format!("is {size} bytes serialized; the limit is {MAX_DATA_BYTES}"),
        ));
    }
    let (dedupe_field, run_field) = if cloud {
        ("pulsededupekey", "pulsecausationrun")
    } else {
        ("dedupe_key", "causation_run")
    };
    let mut dedupe_key = optional_string(&mut obj, dedupe_field, MAX_DEDUPE_KEY_BYTES)?;
    if cloud && dedupe_key.is_none() {
        // CloudEvents producers retry with the same id; treat it as the dedupe key.
        dedupe_key = optional_string(&mut obj, "id", MAX_DEDUPE_KEY_BYTES)?;
    }
    let causation_run = match obj.remove(run_field) {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            serde_json::from_value::<RunId>(v)
                .map_err(|e| err(run_field, format!("is not a run id: {e}")))?,
        ),
    };
    Ok(IngestEvent { type_, subject, time, data, dedupe_key, causation_run })
}

fn optional_string(
    obj: &mut Map<String, Value>,
    field: &str,
    max: usize,
) -> Result<Option<String>, FieldError> {
    match obj.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.len() <= max => Ok(Some(s)),
        Some(Value::String(s)) => {
            Err(err(field, format!("is {} bytes; the limit is {max}", s.len())))
        }
        Some(_) => Err(err(field, "must be a string")),
    }
}

/// Checks an external event's `type` as a sender wrote it: syntax, size, and that it is not in
/// a built-in namespace. `POST /ingest` and `events.ingest` both use it, so a type is refused
/// the same way on either path instead of being renamed on one of them.
pub fn check_type(t: &str) -> Result<(), String> {
    if t.len() > MAX_TYPE_BYTES {
        return Err(format!("is {} bytes; the limit is {MAX_TYPE_BYTES}", t.len()));
    }
    let segments: Vec<&str> = t.split('.').collect();
    let valid = |s: &&str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    if !segments.iter().all(valid) {
        return Err(format!(
            "{:?} is not a valid type: use dot-separated lowercase segments like \"build.failed\"",
            bounded(t, 64)
        ));
    }
    match segments[..] {
        ["ext"] | ["ext", _] => {
            Err("an `ext.` type needs a source and a name: ext.<source>.<name>".into())
        }
        ["ext", ..] => Ok(()),
        [first, ..] if RESERVED.contains(&first) => Err(format!(
            "{first}.* is a built-in namespace; external events are always ext.<source>.<name>, so name it something else (for example \"ci.{t}\")"
        )),
        _ => Ok(()),
    }
}

fn bounded(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(ct: Option<&str>, body: &str) -> Vec<IngestEvent> {
        parse(ct, body.as_bytes(), &IngestLimits::default())
            .unwrap_or_else(|e| panic!("{body}: {e}"))
    }

    fn fails(ct: Option<&str>, body: &str) -> IngestError {
        parse(ct, body.as_bytes(), &IngestLimits::default()).unwrap_err()
    }

    #[test]
    fn shapes() {
        let one = ok(
            Some("application/json"),
            r#"{"type":"build.failed","subject":"main","data":{"job":7}}"#,
        );
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].type_, "build.failed");
        assert_eq!(one[0].subject.as_deref(), Some("main"));
        assert_eq!(one[0].data["job"], 7);

        let arr =
            ok(None, r#"[{"type":"a.b"},{"type":"ext.ci.deploy","time":"2026-11-16T09:00:00Z"}]"#);
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1].time, Some(Timestamp::parse_rfc3339("2026-11-16T09:00:00Z").unwrap()));

        let body = "{\"type\":\"a\"}\n\n{\"type\":\"b\",\"dedupe_key\":\"k\"}\r\n";
        for ct in [Some("application/x-ndjson"), None, Some("application/json")] {
            let lines = ok(ct, body);
            assert_eq!(lines.len(), 2, "{ct:?}");
            assert_eq!(lines[1].dedupe_key.as_deref(), Some("k"));
        }

        let ce = ok(
            Some("application/cloudevents+json; charset=utf-8"),
            r#"{"specversion":"1.0","id":"evt-42","source":"https://ci.example.com","type":"build.failed","subject":"pr/12","time":"2026-11-16T09:41:07.123Z","datacontenttype":"application/json","data":{"ok":false},"comexampleext":"ignored"}"#,
        );
        assert_eq!(ce[0].dedupe_key.as_deref(), Some("evt-42"));
        assert_eq!(ce[0].data["ok"], false);
        let ce = ok(
            Some("application/cloudevents-batch+json"),
            r#"[{"specversion":"1.0","id":"1","source":"x","type":"a.b","pulsededupekey":"mine","pulsecausationrun":null}]"#,
        );
        assert_eq!(ce[0].dedupe_key.as_deref(), Some("mine"));
        // A UTF-8 byte order mark is fine.
        assert_eq!(ok(None, "\u{feff}{\"type\":\"a\"}").len(), 1);
    }

    #[test]
    fn namespace_enforcement() {
        for t in [
            "fs.file.created",
            "git.push.completed",
            "process.exited",
            "schedule.fired",
            "calendar.event.starting",
            "pulse.anything",
        ] {
            let e = fails(None, &format!(r#"{{"type":"{t}"}}"#));
            assert!(e.to_string().contains("built-in namespace"), "{t}: {e}");
        }
        for t in ["ext", "ext.ci", "Build.Failed", "a..b", "a.b.", "", "a b"] {
            assert!(
                matches!(fails(None, &format!(r#"{{"type":"{t}"}}"#)), IngestError::Invalid(_)),
                "{t}"
            );
        }
        // Built-in names are fine below an ext source, and as later segments.
        ok(None, r#"{"type":"ext.ci.fs.file.created"}"#);
        ok(None, r#"{"type":"ci.git.push"}"#);
        ok(None, r#"{"type":"deploy"}"#);
    }

    #[test]
    fn limits_and_precise_errors() {
        let limits = IngestLimits { max_body_bytes: 64, max_events: 2 };
        let big = format!(r#"{{"type":"a","data":"{}"}}"#, "x".repeat(100));
        assert!(matches!(
            parse(None, big.as_bytes(), &limits),
            Err(IngestError::TooLarge { limit: 64, .. })
        ));
        let three = r#"[{"type":"a"},{"type":"a"},{"type":"a"}]"#;
        assert!(matches!(
            parse(None, three.as_bytes(), &limits),
            Err(IngestError::TooMany { count: 3, limit: 2 })
        ));

        let huge_data = format!(r#"{{"type":"a","data":"{}"}}"#, "x".repeat(MAX_DATA_BYTES));
        let long_subject =
            format!(r#"{{"type":"a","subject":"{}"}}"#, "s".repeat(MAX_SUBJECT_BYTES + 1));
        let body = [
            r#"{"type":"ok.one"}"#.to_owned(),
            huge_data,
            long_subject,
            r#"{"type":"a","tpye":"typo"}"#.into(),
            r#"{"type":"a","time":"yesterday"}"#.into(),
            r#"{"type":7}"#.into(),
            r#"{"subject":"no type"}"#.into(),
            r#"["not an object"]"#.into(),
            r#"{"specversion":"0.3","type":"a"}"#.into(),
            r#"{"specversion":"1.0","type":"a","data_base64":"AAEC"}"#.into(),
            r#"{"specversion":"1.0","type":"a","pulsechaindepth":0}"#.into(),
            r#"{"type":"a","causation_run":"not-a-ulid"}"#.into(),
        ]
        .join("\n");
        let limits = IngestLimits { max_body_bytes: 1 << 20, max_events: 100 };
        let Err(IngestError::Invalid(errors)) =
            parse(Some("application/x-ndjson"), body.as_bytes(), &limits)
        else {
            panic!("expected item errors")
        };
        let summary: Vec<(usize, Option<usize>, Option<&str>)> =
            errors.iter().map(|e| (e.index, e.line, e.field.as_deref())).collect();
        assert_eq!(
            summary,
            vec![
                (1, Some(2), Some("data")),
                (2, Some(3), Some("subject")),
                (3, Some(4), None),
                (4, Some(5), Some("time")),
                (5, Some(6), Some("type")),
                (6, Some(7), Some("type")),
                (7, Some(8), None),
                (8, Some(9), Some("specversion")),
                (9, Some(10), Some("data_base64")),
                (10, Some(11), Some("pulsechaindepth")),
                (11, Some(12), Some("causation_run")),
            ]
        );
        assert!(errors[2].message.contains("\"tpye\""));
        assert_eq!(errors[2].to_string().split(':').next(), Some("event 3 (line 4)"));
    }

    #[test]
    fn syntax_errors_and_content_types() {
        assert!(matches!(fails(None, "{\"type\": "), IngestError::Parse { line: 1, .. }));
        assert!(matches!(
            fails(Some("application/x-ndjson"), "{\"type\":\"a\"}\n{oops"),
            IngestError::Parse { line: 2, .. }
        ));
        assert!(matches!(fails(None, "   "), IngestError::Empty));
        assert!(matches!(fails(None, "[]"), IngestError::Empty));
        assert!(matches!(
            fails(Some("application/x-www-form-urlencoded"), "a=b"),
            IngestError::ContentType(_)
        ));
        assert!(matches!(
            fails(Some("application/cloudevents-batch+json"), r#"{"type":"a"}"#),
            IngestError::Parse { .. }
        ));
    }
}
