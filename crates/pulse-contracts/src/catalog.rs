//! The v1 event type catalog, as data, plus typed `data` payloads for the built-in types.
//!
//! Every path-like field in a built-in payload is an absolute path. Sources must join
//! repository-relative paths (for example the files of a merge conflict) before emitting.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::event::{EventType, NewEvent, SourceRef};
use crate::time::{Duration, Timestamp};

/// Where in an event a filesystem path lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathField {
    Subject,
    /// A string at `data.<key>`.
    Data(&'static str),
    /// An array of strings at `data.<key>`.
    DataArray(&'static str),
}

impl PathField {
    pub fn describe(self) -> String {
        match self {
            PathField::Subject => "subject".to_owned(),
            PathField::Data(k) => format!("data.{k}"),
            PathField::DataArray(k) => format!("data.{k}[]"),
        }
    }
}

/// One catalog entry. `example` is a JSON `data` payload.
#[derive(Clone, Copy, Debug)]
pub struct CatalogEntry {
    pub type_: &'static str,
    pub description: &'static str,
    pub subject: &'static str,
    pub paths: &'static [PathField],
    pub example: fn() -> Value,
}

/// The wire form of a catalog entry, for `system.event_types`, MCP `list_event_types`, the rule
/// editor and the docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct EventTypeInfo {
    /// A concrete type, or `ext.*` for the external namespace.
    #[serde(rename = "type")]
    pub type_: String,
    pub description: String,
    /// What the `subject` attribute holds.
    pub subject: String,
    /// The fields that carry filesystem paths (checked against permission scopes).
    pub path_fields: Vec<String>,
    #[ts(type = "unknown")]
    pub example: Value,
}

const FS_FILE: &[PathField] = &[PathField::Subject, PathField::Data("path")];
const FS_MOVED: &[PathField] =
    &[PathField::Subject, PathField::Data("path"), PathField::Data("from")];
const FS_BURST: &[PathField] = &[PathField::Subject, PathField::DataArray("sample")];
const GIT_REPO: &[PathField] = &[PathField::Subject, PathField::Data("repo")];
const GIT_CONFLICT: &[PathField] =
    &[PathField::Subject, PathField::Data("repo"), PathField::DataArray("files")];
const PROCESS: &[PathField] = &[PathField::Data("cwd")];

/// The external namespace. External events carry no paths Pulse can vouch for.
pub const EXTERNAL_PATTERN: &str = "ext.*";

pub static CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        type_: "fs.file.created",
        description: "A file appeared and has stopped changing (after the source's quiet period).",
        subject: "the file's path",
        paths: FS_FILE,
        example: || example_fs_file(Some(482_113)),
    },
    CatalogEntry {
        type_: "fs.file.modified",
        description: "An existing file changed and has stopped changing.",
        subject: "the file's path",
        paths: FS_FILE,
        example: || example_fs_file(Some(491_002)),
    },
    CatalogEntry {
        type_: "fs.file.deleted",
        description: "A file was removed.",
        subject: "the file's path",
        paths: FS_FILE,
        example: || example_fs_file(None),
    },
    CatalogEntry {
        type_: "fs.file.moved",
        description: "A file was renamed or moved within the watched tree.",
        subject: "the new path",
        paths: FS_MOVED,
        example: || {
            json!({
                "from": "/home/me/University/Architettura/draft.pdf",
                "path": "/home/me/University/Architettura/lecture-07.pdf",
                "dir": "/home/me/University/Architettura",
                "name": "lecture-07.pdf",
                "ext": "pdf",
                "size": 482_113
            })
        },
    },
    CatalogEntry {
        type_: "fs.burst",
        description: "Many changes under one directory, folded into one event. Carries counts and a small sample, not every path.",
        subject: "the common directory",
        paths: FS_BURST,
        example: || {
            json!({
                "dir": "/home/me/University/dataset",
                "count": 10_412,
                "created": 10_400,
                "modified": 12,
                "deleted": 0,
                "sample": ["/home/me/University/dataset/a/0001.csv", "/home/me/University/dataset/a/0002.csv"]
            })
        },
    },
    CatalogEntry {
        type_: "git.commit.created",
        description: "A commit was created in a watched repository.",
        subject: "the repository path",
        paths: GIT_REPO,
        example: || {
            json!({
                "repo": "/home/me/code/pulse",
                "branch": "main",
                "sha": "3f2a9c1e0b7d4a6f8e5c2b1a0d9e8f7c6b5a4d3e",
                "message": "Fix flaky watcher test",
                "author": "Ada <ada@example.com>"
            })
        },
    },
    CatalogEntry {
        type_: "git.branch.switched",
        description: "HEAD moved to another branch.",
        subject: "the repository path",
        paths: GIT_REPO,
        example: || json!({ "repo": "/home/me/code/pulse", "from": "main", "to": "feature/ingest" }),
    },
    CatalogEntry {
        type_: "git.merge.conflicted",
        description: "A merge, rebase or cherry-pick stopped with conflicts.",
        subject: "the repository path",
        paths: GIT_CONFLICT,
        example: || {
            json!({
                "repo": "/home/me/code/pulse",
                "files": ["/home/me/code/pulse/src/main.rs", "/home/me/code/pulse/Cargo.toml"]
            })
        },
    },
    CatalogEntry {
        type_: "git.push.completed",
        description: "A push from this machine completed.",
        subject: "the repository path",
        paths: GIT_REPO,
        example: || {
            json!({
                "repo": "/home/me/code/pulse",
                "remote": "origin",
                "branch": "main",
                "sha": "3f2a9c1e0b7d4a6f8e5c2b1a0d9e8f7c6b5a4d3e"
            })
        },
    },
    CatalogEntry {
        type_: "process.exited",
        description: "A command wrapped with `pulse run -- <command>` exited.",
        subject: "the command line",
        paths: PROCESS,
        example: || {
            json!({
                "command": ["cargo", "test"],
                "cwd": "/home/me/code/pulse",
                "exit_code": 101,
                "success": false,
                "signal": null,
                "duration_ms": 48_211,
                "output_tail": "test result: FAILED. 41 passed; 1 failed"
            })
        },
    },
    CatalogEntry {
        type_: "schedule.fired",
        description: "A schedule reached its time.",
        subject: "the schedule (source) name",
        paths: &[],
        example: || json!({ "cron": "0 9 * * 1-5", "every": null, "scheduled_for": "2026-11-16T09:00:00.000Z", "late": false }),
    },
    CatalogEntry {
        type_: "calendar.event.starting",
        description: "A calendar event starts within the source's lead time.",
        subject: "the event title",
        paths: &[],
        example: example_calendar,
    },
    CatalogEntry {
        type_: "calendar.event.ended",
        description: "A calendar event ended.",
        subject: "the event title",
        paths: &[],
        example: example_calendar,
    },
    CatalogEntry {
        type_: EXTERNAL_PATTERN,
        description: "Anything ingested from outside (`pulse ingest`, `POST /ingest`). Always `ext.<source>.<name>`; external sources cannot emit other types.",
        subject: "supplied by the sender",
        paths: &[],
        example: || json!({ "anything": "supplied by the sender" }),
    },
];

fn example_fs_file(size: Option<u64>) -> Value {
    json!({
        "path": "/home/me/University/Architettura/lecture-07.pdf",
        "dir": "/home/me/University/Architettura",
        "name": "lecture-07.pdf",
        "ext": "pdf",
        "size": size
    })
}

fn example_calendar() -> Value {
    json!({
        "uid": "4f1c2e@calendar.example.com",
        "title": "Architettura degli elaboratori",
        "start": "2026-11-16T10:00:00.000Z",
        "end": "2026-11-16T12:00:00.000Z",
        "location": "Aula B",
        "minutes_until": 10
    })
}

/// The catalog entry for a type. `ext.*` types resolve to the external entry.
pub fn entry(type_: &str) -> Option<&'static CatalogEntry> {
    if type_.starts_with("ext.") {
        return CATALOG.iter().find(|e| e.type_ == EXTERNAL_PATTERN);
    }
    CATALOG.iter().find(|e| e.type_ == type_)
}

pub fn event_types() -> Vec<EventTypeInfo> {
    CATALOG
        .iter()
        .map(|e| EventTypeInfo {
            type_: e.type_.to_owned(),
            description: e.description.to_owned(),
            subject: e.subject.to_owned(),
            path_fields: e.paths.iter().map(|p| p.describe()).collect(),
            example: (e.example)(),
        })
        .collect()
}

/// Every filesystem path an event carries, as the catalog defines it. Unknown and external types
/// carry none. Values that are missing or not strings are skipped; the permission check treats
/// any returned value that is not a clean absolute path as out of scope.
pub fn event_paths<'a>(
    type_: &EventType,
    subject: Option<&'a str>,
    data: &'a Value,
) -> Vec<&'a str> {
    let Some(entry) = entry(type_.as_str()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for field in entry.paths {
        match *field {
            PathField::Subject => out.extend(subject),
            PathField::Data(k) => out.extend(data.get(k).and_then(Value::as_str)),
            PathField::DataArray(k) => {
                if let Some(items) = data.get(k).and_then(Value::as_array) {
                    out.extend(items.iter().filter_map(Value::as_str));
                }
            }
        }
    }
    out
}

// ---- Typed payloads for the built-in types. ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct FsFileData {
    pub path: String,
    pub dir: String,
    pub name: String,
    pub ext: Option<String>,
    /// Absent for deletions.
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct FsMovedData {
    pub from: String,
    pub path: String,
    pub dir: String,
    pub name: String,
    pub ext: Option<String>,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FsBurstData {
    pub dir: String,
    pub count: u64,
    pub created: u64,
    pub modified: u64,
    pub deleted: u64,
    /// A small, bounded sample of affected paths.
    pub sample: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct GitCommitData {
    pub repo: String,
    /// `None` on a detached HEAD.
    pub branch: Option<String>,
    pub sha: String,
    pub message: String,
    pub author: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct GitBranchSwitchedData {
    pub repo: String,
    pub from: Option<String>,
    pub to: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GitMergeConflictedData {
    pub repo: String,
    /// Absolute paths of conflicted files.
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GitPushCompletedData {
    pub repo: String,
    pub remote: String,
    pub branch: String,
    pub sha: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct ProcessExitedData {
    pub command: Vec<String>,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub signal: Option<i32>,
    pub duration_ms: u64,
    /// A bounded tail of combined stdout and stderr.
    pub output_tail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct ScheduleFiredData {
    pub cron: Option<String>,
    pub every: Option<Duration>,
    pub scheduled_for: Timestamp,
    /// A catch-up for a time that passed while the machine slept or the daemon was down. At most
    /// one per schedule, for the latest missed time.
    #[serde(default)]
    pub late: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct CalendarEventData {
    pub uid: String,
    pub title: String,
    pub start: Timestamp,
    pub end: Timestamp,
    pub location: Option<String>,
    /// Negative once the event has started.
    pub minutes_until: i64,
}

/// A built-in event with its typed payload. Sources build one of these and call
/// [`BuiltinEvent::into_new_event`], which sets the type and subject the catalog prescribes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuiltinEvent {
    FsFileCreated(FsFileData),
    FsFileModified(FsFileData),
    FsFileDeleted(FsFileData),
    FsFileMoved(FsMovedData),
    FsBurst(FsBurstData),
    GitCommitCreated(GitCommitData),
    GitBranchSwitched(GitBranchSwitchedData),
    GitMergeConflicted(GitMergeConflictedData),
    GitPushCompleted(GitPushCompletedData),
    ProcessExited(ProcessExitedData),
    ScheduleFired(ScheduleFiredData),
    CalendarEventStarting(CalendarEventData),
    CalendarEventEnded(CalendarEventData),
}

impl BuiltinEvent {
    pub fn type_name(&self) -> &'static str {
        match self {
            BuiltinEvent::FsFileCreated(_) => "fs.file.created",
            BuiltinEvent::FsFileModified(_) => "fs.file.modified",
            BuiltinEvent::FsFileDeleted(_) => "fs.file.deleted",
            BuiltinEvent::FsFileMoved(_) => "fs.file.moved",
            BuiltinEvent::FsBurst(_) => "fs.burst",
            BuiltinEvent::GitCommitCreated(_) => "git.commit.created",
            BuiltinEvent::GitBranchSwitched(_) => "git.branch.switched",
            BuiltinEvent::GitMergeConflicted(_) => "git.merge.conflicted",
            BuiltinEvent::GitPushCompleted(_) => "git.push.completed",
            BuiltinEvent::ProcessExited(_) => "process.exited",
            BuiltinEvent::ScheduleFired(_) => "schedule.fired",
            BuiltinEvent::CalendarEventStarting(_) => "calendar.event.starting",
            BuiltinEvent::CalendarEventEnded(_) => "calendar.event.ended",
        }
    }

    /// Builds the `NewEvent`. `source` is the emitting source; schedules use its name as subject.
    pub fn into_new_event(self, source: SourceRef, time: Timestamp) -> NewEvent {
        let type_ = EventType(self.type_name().to_owned());
        let (subject, data) = match self {
            BuiltinEvent::FsFileCreated(d)
            | BuiltinEvent::FsFileModified(d)
            | BuiltinEvent::FsFileDeleted(d) => (Some(d.path.clone()), to_value(&d)),
            BuiltinEvent::FsFileMoved(d) => (Some(d.path.clone()), to_value(&d)),
            BuiltinEvent::FsBurst(d) => (Some(d.dir.clone()), to_value(&d)),
            BuiltinEvent::GitCommitCreated(d) => (Some(d.repo.clone()), to_value(&d)),
            BuiltinEvent::GitBranchSwitched(d) => (Some(d.repo.clone()), to_value(&d)),
            BuiltinEvent::GitMergeConflicted(d) => (Some(d.repo.clone()), to_value(&d)),
            BuiltinEvent::GitPushCompleted(d) => (Some(d.repo.clone()), to_value(&d)),
            BuiltinEvent::ProcessExited(d) => (Some(d.command.join(" ")), to_value(&d)),
            BuiltinEvent::ScheduleFired(d) => (Some(source.name.clone()), to_value(&d)),
            BuiltinEvent::CalendarEventStarting(d) | BuiltinEvent::CalendarEventEnded(d) => {
                (Some(d.title.clone()), to_value(&d))
            }
        };
        NewEvent {
            type_,
            source,
            subject,
            time,
            data,
            dedupe_key: None,
            coalesced: None,
            causation_run: None,
        }
    }
}

fn to_value<T: Serialize>(v: &T) -> Value {
    // These payloads are plain structs of strings, numbers and options: serializing them to a
    // `Value` cannot fail.
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The `EventType` of a built-in catalog entry.
pub fn builtin_type(name: &str) -> Option<EventType> {
    // Built-in names in the catalog are valid by construction (checked by a test).
    CATALOG
        .iter()
        .find(|e| e.type_ == name && e.type_ != EXTERNAL_PATTERN)
        .map(|e| EventType(e.type_.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SourceKind;

    #[test]
    fn catalog_types_are_valid_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for e in CATALOG {
            assert!(seen.insert(e.type_), "duplicate {}", e.type_);
            if e.type_ != EXTERNAL_PATTERN {
                EventType::new(e.type_).unwrap();
            }
            assert!((e.example)().is_object(), "{} example is an object", e.type_);
        }
    }

    #[test]
    fn examples_match_typed_payloads() {
        fn check<T: serde::de::DeserializeOwned + Serialize>(name: &str) {
            let ex = (entry(name).unwrap().example)();
            let typed: T = serde_json::from_value(ex.clone())
                .unwrap_or_else(|e| panic!("{name} example does not fit its payload type: {e}"));
            assert_eq!(serde_json::to_value(&typed).unwrap(), ex, "{name} round trip");
        }
        check::<FsFileData>("fs.file.created");
        check::<FsFileData>("fs.file.deleted");
        check::<FsMovedData>("fs.file.moved");
        check::<FsBurstData>("fs.burst");
        check::<GitCommitData>("git.commit.created");
        check::<GitBranchSwitchedData>("git.branch.switched");
        check::<GitMergeConflictedData>("git.merge.conflicted");
        check::<GitPushCompletedData>("git.push.completed");
        check::<ProcessExitedData>("process.exited");
        check::<ScheduleFiredData>("schedule.fired");
        check::<CalendarEventData>("calendar.event.starting");
    }

    #[test]
    fn paths_per_type() {
        let ty = |s: &str| EventType::new(s).unwrap();
        let cases: Vec<(&str, Option<&str>, Value, Vec<&str>)> = vec![
            (
                "fs.file.created",
                Some("/a/b.pdf"),
                json!({"path": "/a/b.pdf", "dir": "/a"}),
                vec!["/a/b.pdf", "/a/b.pdf"],
            ),
            (
                "fs.file.moved",
                Some("/a/new"),
                json!({"path": "/a/new", "from": "/secret/old"}),
                vec!["/a/new", "/a/new", "/secret/old"],
            ),
            (
                "fs.burst",
                Some("/a"),
                json!({"sample": ["/a/1", "/a/2", 3]}),
                vec!["/a", "/a/1", "/a/2"],
            ),
            (
                "git.merge.conflicted",
                Some("/r"),
                json!({"repo": "/r", "files": ["/r/x"]}),
                vec!["/r", "/r", "/r/x"],
            ),
            ("process.exited", Some("cargo test"), json!({"cwd": "/w"}), vec!["/w"]),
            ("schedule.fired", Some("morning"), json!({}), vec![]),
            ("ext.ci.done", Some("/etc/passwd"), json!({"path": "/etc/passwd"}), vec![]),
            ("unknown.type", Some("/x"), json!({"path": "/x"}), vec![]),
        ];
        for (t, subject, data, want) in cases {
            assert_eq!(event_paths(&ty(t), subject, &data), want, "{t}");
        }
    }

    #[test]
    fn builtin_events_set_type_and_subject() {
        let src = SourceRef::new(SourceKind::Schedule, "morning");
        let ev = BuiltinEvent::ScheduleFired(ScheduleFiredData {
            cron: Some("0 9 * * *".into()),
            every: None,
            scheduled_for: Timestamp::from_millis(0),
            late: false,
        })
        .into_new_event(src, Timestamp::from_millis(5));
        assert_eq!(ev.type_.as_str(), "schedule.fired");
        assert_eq!(ev.subject.as_deref(), Some("morning"));
        for e in CATALOG.iter().filter(|e| e.type_ != EXTERNAL_PATTERN) {
            assert!(builtin_type(e.type_).is_some());
        }
    }
}
