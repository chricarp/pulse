//! A keyed trailing debouncer with a single next deadline, in bounded memory.

use std::collections::{BTreeMap, BTreeSet};

use pulse_contracts::Timestamp;

/// Each key holds the latest value and a deadline that moves with every push. The caller arms
/// one timer for [`Debouncer::next_deadline`] and calls [`Debouncer::pop_due`] when it fires.
#[derive(Clone, Debug)]
pub struct Debouncer<K: Ord + Clone, V> {
    capacity: usize,
    entries: BTreeMap<K, (Timestamp, u64, V)>,
    queue: BTreeSet<(Timestamp, u64, K)>,
    seq: u64,
}

/// The debouncer is full and `key` is not in it. The value is handed back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Full<V>(pub V);

impl<K: Ord + Clone, V> Debouncer<K, V> {
    pub fn new(capacity: usize) -> Self {
        Debouncer {
            capacity: capacity.max(1),
            entries: BTreeMap::new(),
            queue: BTreeSet::new(),
            seq: 0,
        }
    }

    /// Sets `key`'s value and deadline, replacing any pending one.
    pub fn push(&mut self, key: K, value: V, deadline: Timestamp) -> Result<(), Full<V>> {
        if let Some((old_deadline, old_seq, _)) = self.entries.get(&key) {
            self.queue.remove(&(*old_deadline, *old_seq, key.clone()));
        } else if self.entries.len() >= self.capacity {
            return Err(Full(value));
        }
        self.seq += 1;
        self.queue.insert((deadline, self.seq, key.clone()));
        self.entries.insert(key, (deadline, self.seq, value));
        Ok(())
    }

    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    pub fn deadline(&self, key: &K) -> Option<Timestamp> {
        self.entries.get(key).map(|(d, _, _)| *d)
    }

    pub fn next_deadline(&self) -> Option<Timestamp> {
        self.queue.first().map(|(d, _, _)| *d)
    }

    /// Removes and returns every entry whose deadline is at or before `now`, earliest first.
    pub fn pop_due(&mut self, now: Timestamp) -> Vec<(K, V)> {
        let mut out = Vec::new();
        while let Some((deadline, _, _)) = self.queue.first() {
            if *deadline > now {
                break;
            }
            let Some((_, _, key)) = self.queue.pop_first() else { break };
            if let Some((_, _, value)) = self.entries.remove(&key) {
                out.push((key, value));
            }
        }
        out
    }

    /// Drops entries whose key fails `keep` (for example, of a deleted rule).
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        let entries = std::mem::take(&mut self.entries);
        for (k, (d, s, v)) in entries {
            if keep(&k) {
                self.entries.insert(k, (d, s, v));
            } else {
                self.queue.remove(&(d, s, k));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_keyed_and_bounded() {
        let t = Timestamp::from_millis;
        let mut d: Debouncer<&str, u32> = Debouncer::new(2);
        assert_eq!(d.next_deadline(), None);
        d.push("a", 1, t(100)).unwrap();
        d.push("b", 1, t(50)).unwrap();
        assert_eq!(d.next_deadline(), Some(t(50)));
        // A push moves the key's deadline and replaces its value.
        d.push("b", 2, t(150)).unwrap();
        assert_eq!(d.next_deadline(), Some(t(100)));
        assert_eq!(d.push("c", 9, t(10)), Err(Full(9)));
        assert_eq!(d.pop_due(t(99)), vec![]);
        assert_eq!(d.pop_due(t(100)), vec![("a", 1)]);
        d.push("c", 3, t(120)).unwrap();
        assert_eq!(d.pop_due(t(1000)), vec![("c", 3), ("b", 2)]);
        assert!(d.is_empty() && d.next_deadline().is_none());
        d.push("x", 1, t(5)).unwrap();
        d.push("y", 1, t(6)).unwrap();
        d.retain(|k| *k != "x");
        assert_eq!(d.next_deadline(), Some(t(6)));
    }
}
