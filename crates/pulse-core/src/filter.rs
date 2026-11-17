//! In-memory evaluation of an [`EventFilter`], for live subscriptions. It follows the same
//! semantics as the store's query for history (`pulse_store::Queries::query_events`), so a
//! subscription and a query with the same filter agree:
//!
//! - `types`: any of the type globs; `sources`: a full `pulse://` URI or a bare source name;
//! - `subject`: a text glob over the subject (events without a subject never match);
//! - `path`: an event path equal to or under the directory;
//! - `text`: a substring of the subject or the serialized data, ignoring ASCII case (like
//!   SQLite's `LIKE`);
//! - `since` / `until` (inclusive) on `time`, and `caused_by` on the causing run.

use pulse_contracts::pattern::{Glob, PatternError, parse_text_glob, parse_type_glob};
use pulse_contracts::rpc::EventFilter;
use pulse_contracts::{Event, RunId, Timestamp};

use crate::glob;
use crate::path::is_within;

#[derive(Clone, Debug)]
pub struct CompiledFilter {
    types: Vec<Glob>,
    sources: Vec<String>,
    subject: Option<Glob>,
    path: Option<String>,
    text: Option<String>,
    since: Option<Timestamp>,
    until: Option<Timestamp>,
    caused_by: Option<RunId>,
}

impl CompiledFilter {
    pub fn compile(f: &EventFilter) -> Result<CompiledFilter, PatternError> {
        Ok(CompiledFilter {
            types: f.types.iter().map(|t| parse_type_glob(t.as_str())).collect::<Result<_, _>>()?,
            sources: f.sources.clone(),
            subject: f.subject.as_deref().map(parse_text_glob).transpose()?,
            path: f.path.as_deref().map(|p| {
                let trimmed = p.trim_end_matches('/');
                if trimmed.is_empty() { "/".to_owned() } else { trimmed.to_owned() }
            }),
            text: f.text.as_ref().map(|t| t.to_ascii_lowercase()),
            since: f.since,
            until: f.until,
            caused_by: f.caused_by,
        })
    }

    pub fn matches(&self, e: &Event) -> bool {
        if !self.types.is_empty() && !self.types.iter().any(|g| glob::matches(g, e.type_.as_str()))
        {
            return false;
        }
        if !self.sources.is_empty() {
            let name = e.source_ref().map(|s| s.name);
            let hit = self.sources.iter().any(|s| {
                if s.contains("://") { *s == e.source } else { name.as_deref() == Some(s) }
            });
            if !hit {
                return false;
            }
        }
        if let Some(g) = &self.subject {
            if !e.subject.as_deref().is_some_and(|s| glob::matches(g, s)) {
                return false;
            }
        }
        if let Some(dir) = &self.path {
            if !e.paths().iter().any(|p| is_within(p, dir)) {
                return false;
            }
        }
        if let Some(text) = &self.text {
            let in_subject =
                e.subject.as_deref().is_some_and(|s| s.to_ascii_lowercase().contains(text));
            if !in_subject && !e.data.to_string().to_ascii_lowercase().contains(text) {
                return false;
            }
        }
        if self.since.is_some_and(|t| e.time < t) || self.until.is_some_and(|t| e.time > t) {
            return false;
        }
        if self.caused_by.is_some() && e.causation_run != self.caused_by {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use pulse_contracts::config::TypeGlob;
    use pulse_contracts::{EventId, EventType, Ulid};
    use serde_json::json;

    use super::*;

    fn event(type_: &str, source: &str, subject: Option<&str>, time: i64) -> Event {
        Event {
            specversion: Default::default(),
            id: EventId::from_ulid(Ulid::from_parts(time as u64, 0)),
            type_: EventType::new(type_).unwrap(),
            source: source.into(),
            subject: subject.map(str::to_owned),
            time: Timestamp::from_millis(time),
            datacontenttype: None,
            data: json!({ "path": subject, "note": "Build FAILED" }),
            dedupe_key: None,
            coalesced: None,
            causation_run: None,
            causation: None,
            chain_depth: 0,
            inference: None,
            replay_of: None,
        }
    }

    #[test]
    fn filter_table() {
        let pdf = event("fs.file.created", "pulse://fs/uni", Some("/u/a/l.pdf"), 10);
        let glob = |s: &str| s.parse::<TypeGlob>().unwrap();
        let cases: Vec<(EventFilter, bool)> = vec![
            (EventFilter::default(), true),
            (EventFilter { types: vec![glob("fs.*")], ..Default::default() }, true),
            (
                EventFilter { types: vec![glob("git.*"), glob("fs.file.*")], ..Default::default() },
                true,
            ),
            (EventFilter { types: vec![glob("git.*")], ..Default::default() }, false),
            (EventFilter { sources: vec!["uni".into()], ..Default::default() }, true),
            (EventFilter { sources: vec!["pulse://fs/uni".into()], ..Default::default() }, true),
            (EventFilter { sources: vec!["pulse://git/uni".into()], ..Default::default() }, false),
            (EventFilter { subject: Some("*.pdf".into()), ..Default::default() }, true),
            (EventFilter { subject: Some("*.txt".into()), ..Default::default() }, false),
            (EventFilter { path: Some("/u".into()), ..Default::default() }, true),
            (EventFilter { path: Some("/u/".into()), ..Default::default() }, true),
            (EventFilter { path: Some("/u/a/l.pdf".into()), ..Default::default() }, true),
            (EventFilter { path: Some("/uu".into()), ..Default::default() }, false),
            (EventFilter { text: Some("build failed".into()), ..Default::default() }, true),
            (EventFilter { text: Some("L.PDF".into()), ..Default::default() }, true),
            (EventFilter { text: Some("nope".into()), ..Default::default() }, false),
            (EventFilter { since: Some(Timestamp::from_millis(10)), ..Default::default() }, true),
            (EventFilter { since: Some(Timestamp::from_millis(11)), ..Default::default() }, false),
            (EventFilter { until: Some(Timestamp::from_millis(9)), ..Default::default() }, false),
            (
                EventFilter {
                    caused_by: Some(RunId::from_ulid(Ulid::from_parts(1, 1))),
                    ..Default::default()
                },
                false,
            ),
        ];
        for (filter, want) in cases {
            let compiled = CompiledFilter::compile(&filter).unwrap();
            assert_eq!(compiled.matches(&pdf), want, "{filter:?}");
        }
        let no_subject = event("schedule.fired", "pulse://schedule/s", None, 1);
        let subject = EventFilter { subject: Some("*".into()), ..Default::default() };
        assert!(!CompiledFilter::compile(&subject).unwrap().matches(&no_subject));
        assert!(
            CompiledFilter::compile(&EventFilter {
                subject: Some("{a".into()),
                ..Default::default()
            })
            .is_err()
        );
    }
}
