//! Dedupe by key with a TTL, in bounded memory.

use std::collections::{HashMap, VecDeque};

use pulse_contracts::{Duration, Timestamp};

/// Remembers keys for `ttl`. At capacity, the oldest key is forgotten first. A repeat inside the
/// TTL does not extend it: a key that keeps arriving passes once per TTL.
#[derive(Clone, Debug)]
pub struct Dedupe {
    ttl: Duration,
    capacity: usize,
    expiry: HashMap<String, Timestamp>,
    // Insertion order; with one TTL this is also expiry order.
    order: VecDeque<(String, Timestamp)>,
}

impl Dedupe {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Dedupe { ttl, capacity: capacity.max(1), expiry: HashMap::new(), order: VecDeque::new() }
    }

    /// `true` if `key` is new (the event should pass), `false` if it is a duplicate.
    pub fn check(&mut self, key: &str, now: Timestamp) -> bool {
        self.expire(now);
        if self.expiry.get(key).is_some_and(|&until| until > now) {
            return false;
        }
        while self.expiry.len() >= self.capacity {
            let Some((old, _)) = self.order.pop_front() else { break };
            self.expiry.remove(&old);
        }
        let until = now.saturating_add(self.ttl);
        self.expiry.insert(key.to_owned(), until);
        self.order.push_back((key.to_owned(), until));
        true
    }

    pub fn len(&self) -> usize {
        self.expiry.len()
    }

    pub fn is_empty(&self) -> bool {
        self.expiry.is_empty()
    }

    fn expire(&mut self, now: Timestamp) {
        while let Some((key, until)) = self.order.front() {
            if *until > now {
                break;
            }
            // Only remove the map entry if it is this generation of the key.
            if self.expiry.get(key) == Some(until) {
                self.expiry.remove(key);
            }
            self.order.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_ttl_and_capacity() {
        let t = Timestamp::from_millis;
        let mut d = Dedupe::new(Duration::from_secs(10), 2);
        // (key, now ms, passes)
        let steps = [
            ("a", 0, true),
            ("a", 5_000, false),
            ("a", 9_999, false),
            ("a", 10_000, true),
            ("b", 10_001, true),
            ("c", 10_002, true), // evicts the oldest ("a")
            ("a", 10_003, true),
            ("c", 10_004, false),
        ];
        for (key, now, want) in steps {
            assert_eq!(d.check(key, t(now)), want, "{key} at {now}");
            assert!(d.len() <= 2);
        }
        d.check("z", t(1_000_000));
        assert_eq!(d.len(), 1, "expired keys are dropped");
    }
}
