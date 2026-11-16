//! The pure half of causation: which run, if any, caused an incoming event.
//!
//! - **Exact**: the reporter carried `PULSE_RUN_ID`. The caller looks that run up (in its recent
//!   runs or the store) and passes it as `exact`. An id that names no known run is ignored, so a
//!   forged id cannot borrow another run's chain depth.
//! - **Inferred**: a filesystem or git event (`fs.*`, `git.*`) with a path inside the working
//!   directory of a run that was active at the event's time, or ended at most `grace` before it.
//!   With several candidates the deepest working directory wins, then the latest start.
//!   Inference can be wrong; rate limits and the circuit breaker exist because of that.

use pulse_contracts::{Attribution, Causation, Duration, EventType, NewEvent, RunId, Timestamp};

use crate::path::is_within;

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
    infer_cause(&new.type_, &new.paths(), new.time, runs, grace).map(|run| Attribution {
        run: run.id,
        kind: Causation::Inferred,
        chain_depth: run.chain_depth,
    })
}

/// The inferred cause of an event, if any.
pub fn infer_cause<'a>(
    type_: &EventType,
    paths: &[&str],
    time: Timestamp,
    runs: &'a [RecentRun],
    grace: Duration,
) -> Option<&'a RecentRun> {
    if !matches!(type_.namespace(), "fs" | "git") {
        return None;
    }
    runs.iter()
        .filter(|r| {
            r.started_at <= time && r.ended_at.is_none_or(|end| time <= end.saturating_add(grace))
        })
        .filter_map(|r| {
            let cwd = r.cwd.as_deref()?;
            paths.iter().any(|p| is_within(p, cwd)).then_some((cwd.len(), r.started_at, r))
        })
        .max_by_key(|(depth, started, _)| (*depth, *started))
        .map(|(_, _, r)| r)
}

#[cfg(test)]
mod tests {
    use pulse_contracts::Ulid;

    use super::*;

    fn run(n: u64, cwd: Option<&str>, start: i64, end: Option<i64>) -> RecentRun {
        RecentRun {
            id: RunId::from_ulid(Ulid::from_parts(n, 0)),
            rule: Some(format!("rule{n}")),
            cwd: cwd.map(str::to_owned),
            started_at: Timestamp::from_millis(start),
            ended_at: end.map(Timestamp::from_millis),
            chain_depth: n as u32,
        }
    }

    #[test]
    fn inference_table() {
        let grace = Duration::from_secs(30);
        let runs = vec![
            run(1, Some("/home/me/University"), 1_000, None),
            run(2, Some("/home/me/University/Arch"), 2_000, Some(10_000)),
            run(3, Some("/home/me/code"), 500, Some(5_000)),
            run(4, None, 0, None),
        ];
        // (type, paths, time ms, expected run number)
        let cases: Vec<(&str, Vec<&str>, i64, Option<u64>)> = vec![
            ("fs.file.created", vec!["/home/me/University/a.pdf"], 1_500, Some(1)),
            ("fs.file.created", vec!["/home/me/University/Arch/a.pdf"], 3_000, Some(2)),
            // Run 2 ended at 10s; grace is 30s.
            ("fs.file.created", vec!["/home/me/University/Arch/a.pdf"], 40_000, Some(2)),
            ("fs.file.created", vec!["/home/me/University/Arch/a.pdf"], 40_001, Some(1)),
            ("fs.file.created", vec!["/home/me/University/a.pdf"], 999, None),
            ("git.commit.created", vec!["/home/me/code/pulse"], 4_000, Some(3)),
            ("git.commit.created", vec!["/home/me/code/pulse"], 35_001, None),
            ("fs.file.created", vec!["/home/me/Universityx/a"], 1_500, None),
            ("fs.file.created", vec!["/home/me/University/../code/x"], 1_500, None),
            ("process.exited", vec!["/home/me/University"], 1_500, None),
            ("calendar.event.starting", vec![], 1_500, None),
        ];
        for (ty, paths, time, want) in cases {
            let got = infer_cause(
                &EventType::new(ty).unwrap(),
                &paths,
                Timestamp::from_millis(time),
                &runs,
                grace,
            )
            .map(|r| r.chain_depth as u64);
            assert_eq!(got, want, "{ty} {paths:?} at {time}");
        }
    }

    #[test]
    fn exact_beats_inferred_and_unknown_ids_are_ignored() {
        let grace = Duration::from_secs(30);
        let runs = vec![run(1, Some("/w"), 0, None)];
        let other = run(7, None, 0, None);
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
