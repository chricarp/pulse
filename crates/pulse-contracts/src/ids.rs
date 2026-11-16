//! Typed identifiers. Every id is a ULID: 128 bits, sortable by creation time, written as 26
//! Crockford base32 characters. Event ids double as pagination cursors.
//!
//! This crate only parses and formats ids. The daemon generates them (monotonically, so ids
//! assigned in one process sort in assignment order) with `ulid::Generator`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;

pub use ulid::Ulid;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("expected a {kind} (26-character ULID), got {value:?}")]
pub struct IdParseError {
    pub kind: &'static str,
    pub value: String,
}

macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident, $label:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
        #[ts(type = "string")]
        pub struct $name(Ulid);

        impl $name {
            pub const fn from_ulid(u: Ulid) -> Self {
                $name(u)
            }
            pub const fn ulid(self) -> Ulid {
                self.0
            }
            /// Milliseconds since the epoch encoded in the id.
            pub fn timestamp_ms(self) -> u64 {
                self.0.timestamp_ms()
            }
        }

        impl From<Ulid> for $name {
            fn from(u: Ulid) -> Self {
                $name(u)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s)
                    .map($name)
                    .map_err(|_| IdParseError { kind: $label, value: s.to_owned() })
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = std::borrow::Cow::<str>::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

ulid_id!(
    /// An event in the log.
    EventId,
    "event id"
);
ulid_id!(
    /// One triggered agent execution.
    RunId,
    "run id"
);
ulid_id!(
    /// The recorded routing decision for one (event, matching rule) pair.
    DeliveryId,
    "delivery id"
);
ulid_id!(
    /// A live subscription on one client connection.
    SubscriptionId,
    "subscription id"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_as_strings_and_sort_by_time() {
        let a = EventId::from_ulid(Ulid::from_parts(1_000, 7));
        let b = EventId::from_ulid(Ulid::from_parts(2_000, 1));
        assert!(a < b);
        assert!(a.to_string() < b.to_string());
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<EventId>(&json).unwrap(), a);
        let err = "nope".parse::<RunId>().unwrap_err();
        assert_eq!(err.kind, "run id");
    }
}
