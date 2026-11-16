//! The pure half of causation: which run, if any, caused an incoming event.
//!
//! - **Exact**: the reporter carried `PULSE_RUN_ID`. The caller looks that run up (in its recent
//!   runs or the store) and passes it as `exact`. An id that names no known run is ignored, so a
//!   forged id cannot borrow another run's chain depth.
//! - **Reported**: a filesystem or git event carrying a path that a run active at the event's
//!   time (or ended at most `grace` before it) said it wrote: its adapter saw the tool call. Inside
//!   the run's working directory or not.
//! - **Inferred**: a filesystem or git event with a path inside the working directory of such a
//!   run whose writes are [opaque](RunWrites): one whose adapter cannot report writes, or that
//!   did something whose writes Pulse cannot see (a shell command, an MCP tool), or that was
//!   reloaded after a restart. A run whose writes are all known claims nothing by inference: a
//!   file in its folder that it did not write is the user's.
//!
//! With several candidates, reported beats inferred; among reported, the latest start wins;
//! among inferred, the deepest working directory, then the latest start. Both reported and
//! inferred can be wrong (see `docs/internals/0002-architecture.md`, "Loop protection"); rate
//! limits and the circuit breaker exist because of that.

use std::collections::BTreeSet;

use pulse_contracts::{Attribution, Causation, Duration, EventType, NewEvent, RunId, Timestamp};

use crate::path::{is_clean_absolute, is_within};

/// A run the engine still remembers for attribution: active ones, and ones that ended within
/// the grace period. The caller bounds this list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentRun {
    pub id: RunId,
    pub rule: Option<String>,
    /// Clean absolute working directory. Runs without one are never inferred causes.
    pub cwd: Option<String>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub chain_depth: u32,
    pub writes: RunWrites,
}

/// What a run's adapter has reported about the files the run wrote.
///
/// A run is **observed** while every tool call it made was either known to be read-only or a
/// write that named its path; it is **opaque** from the first call whose writes cannot be seen,
/// when more than [`RunWrites::MAX_PATHS`] paths were reported, or from the start if its
/// adapter cannot report writes at all. Opaque is for good. Written paths are kept either way
/// (up to the bound), so a reported write is attributed exactly even in an opaque run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunWrites {
    /// Clean absolute paths, in every spelling the caller knows for them.
    paths: BTreeSet<String>,
    opaque: bool,
}

impl RunWrites {
    /// Paths (spellings included) remembered per run.
    pub const MAX_PATHS: usize = 128;

    /// Nothing written yet, and every write will be reported.
    pub fn observed() -> RunWrites {
        RunWrites::default()
    }

    /// Writes cannot be told apart: attribution in the working directory is inferred.
    pub fn opaque() -> RunWrites {
        RunWrites { paths: BTreeSet::new(), opaque: true }
    }

    pub fn is_opaque(&self) -> bool {
        self.opaque
    }

    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.paths.iter().map(String::as_str)
    }

    /// Records one write, given as the spellings of its path. An empty or unclean spelling list
    /// (a path the caller could not resolve) makes the run opaque, and so does exceeding the
    /// bound. Returns whether this made the run opaque.
    pub fn record_write(&mut self, spellings: &[String]) -> bool {
        if spellings.is_empty() || !spellings.iter().all(|p| is_clean_absolute(p)) {
            return self.make_opaque();
        }
        let new = spellings.iter().filter(|p| !self.paths.contains(*p)).count();
        if self.paths.len() + new > Self::MAX_PATHS {
            return self.make_opaque();
        }
        self.paths.extend(spellings.iter().cloned());
        false
    }

    /// Something happened whose writes cannot be seen. Returns whether the run was observed.
    pub fn make_opaque(&mut self) -> bool {
        !std::mem::replace(&mut self.opaque, true)
    }
}

/// Attributes a new event. `exact` is the run named by `new.causation_run`, if the caller found it.
pub fn attribute(
    new: &NewEvent,
    exact: Option<&RecentRun>,
    runs: &[RecentRun],
    grace: Duration,
) -> Option<Attribution> {
    if let Some(run) = exact.filter(|r| Some(r.id) == new.causation_run) {
        return Some(Attribution {
            run: run.id,
            kind: Causation::Exact,
            chain_depth: run.chain_depth,
        });
    }
    cause_of_change(&new.type_, &new.paths(), new.time, runs, grace)
        .map(|(run, kind)| Attribution { run: run.id, kind, chain_depth: run.chain_depth })
}

/// The cause of a filesystem or git event that did not name its run: `Reported` or `Inferred`.
pub fn cause_of_change<'a>(
    type_: &EventType,
    paths: &[&str],
    time: Timestamp,
    runs: &'a [RecentRun],
    grace: Duration,
) -> Option<(&'a RecentRun, Causation)> {
    if !matches!(type_.namespace(), "fs" | "git") {
        return None;
    }
    let live = runs.iter().filter(|r| {
        r.started_at <= time && r.ended_at.is_none_or(|end| time <= end.saturating_add(grace))
    });
    let reported = live
        .clone()
        .filter(|r| paths.iter().any(|p| r.writes.contains(p)))
        .max_by_key(|r| r.started_at);
    if let Some(run) = reported {
        return Some((run, Causation::Reported));
    }
    live.filter(|r| r.writes.is_opaque())
        .filter_map(|r| {
            let cwd = r.cwd.as_deref()?;
            paths.iter().any(|p| is_within(p, cwd)).then_some((cwd.len(), r.started_at, r))
        })
        .max_by_key(|(depth, started, _)| (*depth, *started))
        .map(|(_, _, r)| (r, Causation::Inferred))
}

#[cfg(test)]
mod tests {
    use pulse_contracts::Ulid;

    use super::*;

    /// Run `n` has chain depth `n`, so a result is identified by its depth.
    fn run(
        n: u64,
        cwd: Option<&str>,
        start: i64,
        end: Option<i64>,
        writes: RunWrites,
    ) -> RecentRun {
        RecentRun {
            id: RunId::from_ulid(Ulid::from_parts(n, 0)),
            rule: Some(format!("rule{n}")),
            cwd: cwd.map(str::to_owned),
            started_at: Timestamp::from_millis(start),
            ended_at: end.map(Timestamp::from_millis),
            chain_depth: n as u32,
            writes,
        }
    }

    fn wrote(paths: &[&str], opaque: bool) -> RunWrites {
        let mut w = RunWrites::observed();
        for p in paths {
            assert!(!w.record_write(&[(*p).to_owned()]));
        }
        if opaque {
            w.make_opaque();
        }
        w
    }

    #[test]
    fn attribution_table() {
        use Causation::{Inferred as I, Reported as R};
        let grace = Duration::from_secs(30);
        let opaque = RunWrites::opaque;
        let runs = vec![
            run(1, Some("/home/me/University"), 1_000, None, opaque()),
            run(2, Some("/home/me/University/Arch"), 2_000, Some(10_000), opaque()),
            run(3, Some("/home/me/code"), 500, Some(5_000), opaque()),
            run(4, None, 0, None, opaque()),
            // Observed: wrote two files, one outside its working directory, and one in run 1's.
            run(
                5,
                Some("/home/me/Notes"),
                1_000,
                None,
                wrote(
                    &["/home/me/Notes/a.md", "/home/me/Out/x.md", "/home/me/University/s.md"],
                    false,
                ),
            ),
            // Observed, ended at 5s.
            run(
                6,
                Some("/home/me/repo"),
                1_000,
                Some(5_000),
                wrote(&["/home/me/repo/README"], false),
            ),
            // Opaque (it ran a shell command), but also reported a write.
            run(7, Some("/home/me/Mixed"), 1_000, None, wrote(&["/home/me/Else/y.md"], true)),
        ];
        // (type, paths, time ms, expected (run, causation))
        type Case<'a> = (&'a str, Vec<&'a str>, i64, Option<(u64, Causation)>);
        let cases: Vec<Case> = vec![
            // Inference, for opaque runs: as before.
            ("fs.file.created", vec!["/home/me/University/a.pdf"], 1_500, Some((1, I))),
            ("fs.file.created", vec!["/home/me/University/Arch/a.pdf"], 3_000, Some((2, I))),
            // Run 2 ended at 10s; grace is 30s.
            ("fs.file.created", vec!["/home/me/University/Arch/a.pdf"], 40_000, Some((2, I))),
            ("fs.file.created", vec!["/home/me/University/Arch/a.pdf"], 40_001, Some((1, I))),
            ("fs.file.created", vec!["/home/me/University/a.pdf"], 999, None),
            ("git.commit.created", vec!["/home/me/code/pulse"], 4_000, Some((3, I))),
            ("git.commit.created", vec!["/home/me/code/pulse"], 35_001, None),
            ("fs.file.created", vec!["/home/me/Universityx/a"], 1_500, None),
            ("fs.file.created", vec!["/home/me/University/../code/x"], 1_500, None),
            ("process.exited", vec!["/home/me/University"], 1_500, None),
            ("calendar.event.starting", vec![], 1_500, None),
            ("fs.file.created", vec!["/home/me/Mixed/z"], 1_500, Some((7, I))),
            // An observed run claims only what it wrote: the rest of its folder is the user's.
            ("fs.file.created", vec!["/home/me/Notes/b.pdf"], 1_500, None),
            ("fs.file.created", vec!["/home/me/Notes/a.md"], 1_500, Some((5, R))),
            ("fs.file.created", vec!["/home/me/Notes/a.md"], 999, None),
            // Outside its working directory too, and in an opaque run.
            ("fs.file.modified", vec!["/home/me/Out/x.md"], 1_500, Some((5, R))),
            ("fs.file.modified", vec!["/home/me/Else/y.md"], 1_500, Some((7, R))),
            // Reported beats another run's inference.
            ("fs.file.created", vec!["/home/me/University/s.md"], 1_500, Some((5, R))),
            // Any of the event's paths (a move's `from`, a burst's sample).
            (
                "fs.file.moved",
                vec!["/home/me/Notes/n.md", "/home/me/Notes/a.md"],
                1_500,
                Some((5, R)),
            ),
            // A commit in an observed run's folder is the user's; a reported write keeps the
            // grace period.
            ("git.commit.created", vec!["/home/me/repo"], 2_000, None),
            ("fs.file.modified", vec!["/home/me/repo/README"], 35_000, Some((6, R))),
            ("fs.file.modified", vec!["/home/me/repo/README"], 35_001, None),
            // Reported paths are only for fs and git events.
            ("process.exited", vec!["/home/me/Notes/a.md"], 1_500, None),
        ];
        for (ty, paths, time, want) in cases {
            let got = cause_of_change(
                &EventType::new(ty).unwrap(),
                &paths,
                Timestamp::from_millis(time),
                &runs,
                grace,
            )
            .map(|(r, kind)| (r.chain_depth as u64, kind));
            assert_eq!(got, want, "{ty} {paths:?} at {time}");
        }
    }

    #[test]
    fn writes_are_bounded_and_opaque_is_for_good() {
        let mut w = RunWrites::observed();
        assert!(!w.record_write(&["/w/a".into(), "/private/w/a".into()]));
        assert!(!w.record_write(&["/w/a".into()]), "a repeated write costs nothing");
        assert!(w.contains("/private/w/a") && !w.is_opaque());
        // A path the caller could not resolve.
        assert!(w.clone().record_write(&[]));
        assert!(w.clone().record_write(&["w/a".into()]));
        for i in 2..RunWrites::MAX_PATHS {
            assert!(!w.record_write(&[format!("/w/{i}")]), "{i}");
        }
        assert!(w.record_write(&["/w/over".into()]), "one past the bound");
        assert!(w.is_opaque() && !w.contains("/w/over") && w.contains("/w/a"));
        assert!(!w.make_opaque(), "already opaque");
        assert!(!w.record_write(&["/w/more".into()]));
        assert_eq!(w.paths().count(), RunWrites::MAX_PATHS);
    }

    #[test]
    fn exact_beats_inferred_and_unknown_ids_are_ignored() {
        let grace = Duration::from_secs(30);
        let runs = vec![run(1, Some("/w"), 0, None, RunWrites::opaque())];
        let other = run(7, None, 0, None, RunWrites::opaque());
        let mut new = NewEvent {
            type_: EventType::new("fs.file.created").unwrap(),
            source: pulse_contracts::SourceRef::new(pulse_contracts::SourceKind::Fs, "w"),
            subject: Some("/w/a".into()),
            time: Timestamp::from_millis(10),
            data: serde_json::json!({"path": "/w/a"}),
            dedupe_key: None,
            coalesced: None,
            causation_run: Some(other.id),
        };
        let a = attribute(&new, Some(&other), &runs, grace).unwrap();
        assert_eq!((a.run, a.kind, a.chain_depth), (other.id, Causation::Exact, 7));
        // The caller did not find the named run: fall back to inference.
        let a = attribute(&new, None, &runs, grace).unwrap();
        assert_eq!((a.run, a.kind), (runs[0].id, Causation::Inferred));
        // A found run that is not the one named is not used as exact.
        new.causation_run = None;
        assert_eq!(attribute(&new, Some(&other), &runs, grace).unwrap().kind, Causation::Inferred);
    }
}
