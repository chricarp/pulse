//! The order `pulse tail` shows things in: an event before the runs it caused.
//!
//! An event and its runs are committed together, but they reach a client on two subscriptions
//! that each send at most every 250 ms, so a run's first change can arrive before its event.
//! A run of an event newer than the log was when the tail started waits until that event has
//! been shown. With filters, the events of some runs are never shown, and neither are those
//! runs: `tail` shows the runs of the events it shows.

use std::collections::{HashSet, VecDeque};

use pulse_contracts::{EventId, RunSummary};

/// Events remembered as shown; a run's event is shown just before it, never this long ago.
const SHOWN: usize = 4096;
/// Run changes waiting for their event; the oldest is dropped beyond this.
const HELD: usize = 1000;

#[derive(Debug)]
pub struct Order {
    /// The newest event in the log when the tail subscribed: runs of earlier events are shown
    /// at once.
    baseline: Option<EventId>,
    shown: VecDeque<EventId>,
    shown_set: HashSet<EventId>,
    held: VecDeque<RunSummary>,
}

impl Order {
    pub fn new(baseline: Option<EventId>) -> Order {
        Order { baseline, shown: VecDeque::new(), shown_set: HashSet::new(), held: VecDeque::new() }
    }

    fn ready(&self, r: &RunSummary) -> bool {
        self.baseline.is_some_and(|b| r.event <= b) || self.shown_set.contains(&r.event)
    }

    /// Events about to be shown (`dropped`: the batch left some out, which may have been the
    /// events of held runs). Returns the held run changes that can follow them, in arrival
    /// order.
    pub fn events(
        &mut self,
        ids: impl IntoIterator<Item = EventId>,
        dropped: bool,
    ) -> Vec<RunSummary> {
        for id in ids {
            if self.shown_set.insert(id) {
                self.shown.push_back(id);
            }
            if self.shown.len() > SHOWN {
                if let Some(old) = self.shown.pop_front() {
                    self.shown_set.remove(&old);
                }
            }
        }
        let held = std::mem::take(&mut self.held);
        let (ready, waiting) = held.into_iter().partition(|r| dropped || self.ready(r));
        self.held = waiting;
        Vec::from_iter::<VecDeque<_>>(ready)
    }

    /// Run changes as they arrive: those that can be shown now. The others wait for their
    /// event.
    pub fn runs(&mut self, runs: Vec<RunSummary>) -> Vec<RunSummary> {
        let mut ready = Vec::new();
        for r in runs {
            // A run already waiting keeps its later changes behind it.
            if self.ready(&r) && !self.held.iter().any(|h| h.id == r.id) {
                ready.push(r);
            } else {
                self.held.push_back(r);
                if self.held.len() > HELD {
                    self.held.pop_front();
                }
            }
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use pulse_contracts::{RunId, RunStatus, Timestamp, Ulid};

    use super::*;

    fn eid(n: u64) -> EventId {
        EventId::from_ulid(Ulid::from_parts(n, 0))
    }

    fn run(n: u64, event: u64, status: RunStatus) -> RunSummary {
        RunSummary {
            id: RunId::from_ulid(Ulid::from_parts(n, 1)),
            rule: Some("r".into()),
            agent: "a".into(),
            event: eid(event),
            status,
            created_at: Timestamp::from_millis(0),
            started_at: None,
            ended_at: None,
            exit_code: None,
            chain_depth: 1,
        }
    }

    #[test]
    fn a_run_waits_for_its_event() {
        use RunStatus::*;
        let mut o = Order::new(Some(eid(10)));
        // Of an event from before the tail started: at once.
        assert_eq!(o.runs(vec![run(1, 9, Running)]), [run(1, 9, Running)]);
        // Of a new event: after it, with its later changes in order.
        assert!(o.runs(vec![run(2, 11, Queued)]).is_empty());
        assert!(o.runs(vec![run(2, 11, Running)]).is_empty());
        assert!(o.events([eid(12)], false).is_empty());
        assert_eq!(o.events([eid(11)], false), [run(2, 11, Queued), run(2, 11, Running)]);
        assert_eq!(o.runs(vec![run(2, 11, Succeeded)]), [run(2, 11, Succeeded)]);
        // A batch that dropped events may have dropped theirs: nothing waits for ever.
        assert!(o.runs(vec![run(3, 13, Queued)]).is_empty());
        assert_eq!(o.events([], true), [run(3, 13, Queued)]);
        // An empty log at the start: every run's event is new.
        let mut o = Order::new(None);
        assert!(o.runs(vec![run(4, 1, Queued)]).is_empty());
        assert_eq!(o.events([eid(1)], false), [run(4, 1, Queued)]);
    }
}
