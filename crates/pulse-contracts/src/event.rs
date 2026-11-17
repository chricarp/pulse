//! The event envelope: CloudEvents 1.0 JSON with `pulse*` extension attributes.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use ts_rs::TS;

use crate::ids::{EventId, RunId};
use crate::time::Timestamp;

/// The CloudEvents `specversion`. Only `"1.0"` exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum SpecVersion {
    #[default]
    #[serde(rename = "1.0")]
    V1_0,
}

/// A stored, routed event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct Event {
    pub specversion: SpecVersion,
    pub id: EventId,
    #[serde(rename = "type")]
    pub type_: EventType,
    /// `pulse://<kind>/<source name>`.
    pub source: String,
    pub subject: Option<String>,
    pub time: Timestamp,
    /// Always `application/json` for events Pulse writes.
    pub datacontenttype: Option<String>,
    #[ts(type = "unknown")]
    pub data: Value,
    #[serde(rename = "pulsededupekey", default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    /// How many raw signals were folded into this event.
    #[serde(rename = "pulsecoalesced", default, skip_serializing_if = "Option::is_none")]
    pub coalesced: Option<u32>,
    /// The run believed to have caused this event.
    #[serde(rename = "pulsecausationrun", default, skip_serializing_if = "Option::is_none")]
    pub causation_run: Option<RunId>,
    #[serde(rename = "pulsecausation", default, skip_serializing_if = "Option::is_none")]
    pub causation: Option<Causation>,
    /// For an `inferred` cause, what Pulse could not see of the run's writes, which is why it
    /// had to guess. Absent on events recorded before it was kept.
    #[serde(rename = "pulseinference", default, skip_serializing_if = "Option::is_none")]
    pub inference: Option<Inference>,
    /// 0 for events with no causing run; otherwise the causing run's chain depth.
    #[serde(rename = "pulsechaindepth", default)]
    pub chain_depth: u32,
    #[serde(rename = "pulsereplayof", default, skip_serializing_if = "Option::is_none")]
    pub replay_of: Option<EventId>,
}

impl Event {
    pub const CONTENT_TYPE: &'static str = "application/json";

    /// Every absolute filesystem path this event carries, per the catalog. Permission checks
    /// require all of them to be in scope. See [`crate::catalog::event_paths`].
    pub fn paths(&self) -> Vec<&str> {
        crate::catalog::event_paths(&self.type_, self.subject.as_deref(), &self.data)
    }

    /// The source reference parsed from `source`, if it is a `pulse://` URI.
    pub fn source_ref(&self) -> Option<SourceRef> {
        SourceRef::parse_uri(&self.source)
    }
}

/// How the causing run of an event is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Causation {
    /// Reported by a process that carried `PULSE_RUN_ID` (`pulse run`, `pulse ingest`).
    Exact,
    /// A filesystem event on a path the run's agent said it wrote: its adapter saw the tool
    /// call (Claude Code's `Write`, `Edit`, …). As good as the agent's own output.
    Reported,
    /// Attributed because a filesystem or git event happened inside the working directory of
    /// an active run whose writes Pulse cannot see. May be wrong.
    Inferred,
}

/// Why the writes of a run are opaque, so that a change in its working directory can only be
/// attributed to it by inference. Each needs a different explanation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Inference {
    /// Its agent's adapter does not report writes at all (`command`, `codex`): Pulse sees none.
    Unreported,
    /// It did something whose writes Pulse cannot see (Claude Code ran a shell command, a
    /// subagent or an MCP tool, or wrote a path Pulse could not resolve).
    Unseen,
    /// It was running when the daemon restarted; its output is no longer read.
    Reloaded,
}

/// The causation attached to an event when the engine accepts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attribution {
    pub run: RunId,
    pub kind: Causation,
    /// Set when `kind` is `Inferred`.
    pub inference: Option<Inference>,
    /// The causing run's chain depth; becomes the event's `pulsechaindepth`.
    pub chain_depth: u32,
}

/// What a source hands to the engine. The engine assigns the id, applies dedupe, attributes
/// causation and turns it into an [`Event`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewEvent {
    #[serde(rename = "type")]
    pub type_: EventType,
    pub source: SourceRef,
    pub subject: Option<String>,
    /// When the source observed the change.
    pub time: Timestamp,
    pub data: Value,
    pub dedupe_key: Option<String>,
    pub coalesced: Option<u32>,
    /// Exact causation: the `PULSE_RUN_ID` the reporting process carried. Sources that watch
    /// the machine leave this `None`; the engine infers causation for them.
    pub causation_run: Option<RunId>,
}

impl NewEvent {
    pub fn into_event(
        self,
        id: EventId,
        attribution: Option<Attribution>,
        replay_of: Option<EventId>,
    ) -> Event {
        Event {
            specversion: SpecVersion::V1_0,
            id,
            type_: self.type_,
            source: self.source.uri(),
            subject: self.subject,
            time: self.time,
            datacontenttype: Some(Event::CONTENT_TYPE.to_owned()),
            data: self.data,
            dedupe_key: self.dedupe_key,
            coalesced: self.coalesced,
            causation_run: attribution.map(|a| a.run),
            causation: attribution.map(|a| a.kind),
            inference: attribution.and_then(|a| a.inference),
            chain_depth: attribution.map_or(0, |a| a.chain_depth),
            replay_of,
        }
    }

    /// Every absolute filesystem path this event carries, per the catalog.
    pub fn paths(&self) -> Vec<&str> {
        crate::catalog::event_paths(&self.type_, self.subject.as_deref(), &self.data)
    }
}

/// Where events come from. Config-defined sources use their own kind; `process` is `pulse run`;
/// `ext` is anything ingested over RPC or HTTP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Fs,
    Git,
    Schedule,
    Calendar,
    Process,
    Ext,
}

impl SourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            SourceKind::Fs => "fs",
            SourceKind::Git => "git",
            SourceKind::Schedule => "schedule",
            SourceKind::Calendar => "calendar",
            SourceKind::Process => "process",
            SourceKind::Ext => "ext",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "fs" => SourceKind::Fs,
            "git" => SourceKind::Git,
            "schedule" => SourceKind::Schedule,
            "calendar" => SourceKind::Calendar,
            "process" => SourceKind::Process,
            "ext" => SourceKind::Ext,
            _ => return None,
        })
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A source's identity: `pulse://<kind>/<name>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    pub kind: SourceKind,
    pub name: String,
}

impl SourceRef {
    pub fn new(kind: SourceKind, name: impl Into<String>) -> Self {
        SourceRef { kind, name: name.into() }
    }

    pub fn uri(&self) -> String {
        format!("pulse://{}/{}", self.kind, self.name)
    }

    pub fn parse_uri(uri: &str) -> Option<SourceRef> {
        let rest = uri.strip_prefix("pulse://")?;
        let (kind, name) = rest.split_once('/')?;
        if name.is_empty() {
            return None;
        }
        Some(SourceRef { kind: SourceKind::parse(kind)?, name: name.to_owned() })
    }
}

/// A validated event type: two or more dot-separated segments of `[a-z0-9_-]`, e.g.
/// `fs.file.created`, `ext.ci.build.failed`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(type = "string")]
pub struct EventType(pub(crate) String);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid event type {0:?}: expected dot-separated lowercase segments like \"fs.file.created\""
)]
pub struct EventTypeError(pub String);

impl EventType {
    pub fn new(s: impl Into<String>) -> Result<Self, EventTypeError> {
        let s = s.into();
        if is_valid_type(&s) { Ok(EventType(s)) } else { Err(EventTypeError(s)) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first segment: `fs`, `git`, `ext`, ...
    pub fn namespace(&self) -> &str {
        self.0.split('.').next().unwrap_or("")
    }

    pub fn is_external(&self) -> bool {
        self.namespace() == "ext"
    }

    /// The type an external source called `source` may emit for `suffix`. External events are
    /// always forced into `ext.<source>.`, so ingest can never impersonate a built-in type.
    pub fn external(source: &str, suffix: &str) -> Result<Self, EventTypeError> {
        let suffix = suffix
            .strip_prefix("ext.")
            .and_then(|s| s.strip_prefix(source))
            .and_then(|s| s.strip_prefix('.'))
            .unwrap_or(suffix);
        EventType::new(format!("ext.{source}.{suffix}"))
    }
}

pub(crate) fn is_valid_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg.len() <= 64
        && seg
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn is_valid_type(s: &str) -> bool {
    s.len() <= 200 && s.split('.').count() >= 2 && s.split('.').all(is_valid_segment)
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl FromStr for EventType {
    type Err = EventTypeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EventType::new(s)
    }
}

impl AsRef<str> for EventType {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for EventType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        EventType::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Ulid;

    #[test]
    fn event_types() {
        for ok in ["fs.file.created", "ext.ci.build-failed", "a.b", "git.push.completed"] {
            assert!(EventType::new(ok).is_ok(), "{ok}");
        }
        for bad in ["fs", "", "Fs.file", "fs..x", "fs.file.", "fs.*", "fs file.x", ".fs"] {
            assert!(EventType::new(bad).is_err(), "{bad:?}");
        }
        let cases = [
            ("ci", "build.failed", "ext.ci.build.failed"),
            ("ci", "ext.ci.build.failed", "ext.ci.build.failed"),
            // Cannot escape the namespace by naming a built-in type or another source.
            ("ci", "fs.file.created", "ext.ci.fs.file.created"),
            ("ci", "ext.other.x", "ext.ci.ext.other.x"),
        ];
        for (source, suffix, want) in cases {
            assert_eq!(EventType::external(source, suffix).unwrap().as_str(), want);
        }
    }

    #[test]
    fn envelope_uses_cloudevents_attribute_names() {
        let new = NewEvent {
            type_: EventType::new("fs.file.created").unwrap(),
            source: SourceRef::new(SourceKind::Fs, "university"),
            subject: Some("/home/me/U/a.pdf".into()),
            time: Timestamp::from_millis(0),
            data: serde_json::json!({"path": "/home/me/U/a.pdf"}),
            dedupe_key: None,
            coalesced: Some(3),
            causation_run: None,
        };
        let run = RunId::from_ulid(Ulid::from_parts(5, 5));
        let attribution = Attribution {
            run,
            kind: Causation::Inferred,
            inference: Some(Inference::Unreported),
            chain_depth: 2,
        };
        let ev =
            new.into_event(EventId::from_ulid(Ulid::from_parts(9, 9)), Some(attribution), None);
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["specversion"], "1.0");
        assert_eq!(json["type"], "fs.file.created");
        assert_eq!(json["source"], "pulse://fs/university");
        assert_eq!(json["pulsecoalesced"], 3);
        assert_eq!(json["pulsecausation"], "inferred");
        assert_eq!(json["pulseinference"], "unreported");
        assert_eq!(json["pulsechaindepth"], 2);
        assert!(json.get("pulsereplayof").is_none());
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
        assert_eq!(back.source_ref(), Some(SourceRef::new(SourceKind::Fs, "university")));
    }
}
