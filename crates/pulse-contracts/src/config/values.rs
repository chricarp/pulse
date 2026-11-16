//! Small config value types with their own string syntax: type globs, scopes, rates and the
//! `where` matcher language.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;

use crate::pattern::{PatternError, parse_type_glob};
use crate::time::Duration;

macro_rules! string_serde {
    ($ty:ty) => {
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = std::borrow::Cow::<str>::deserialize(d)?;
                s.parse().map_err(de::Error::custom)
            }
        }
    };
}

/// An event type glob such as `fs.*` or `git.commit.created`. Syntax is checked on parse.
#[derive(Clone, Debug, PartialEq, Eq, Hash, TS)]
#[ts(type = "string")]
pub struct TypeGlob(String);

impl TypeGlob {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for TypeGlob {
    type Err = PatternError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_type_glob(s)?;
        Ok(TypeGlob(s.to_owned()))
    }
}

impl fmt::Display for TypeGlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
string_serde!(TypeGlob);

/// `on = "fs.file.created"` or `on = ["fs.file.created", "fs.file.moved"]`. Always serialized as
/// an array; accepted as either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(transparent)]
pub struct TypeGlobs(pub Vec<TypeGlob>);

impl<'de> Deserialize<'de> for TypeGlobs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        let items = match OneOrMany::deserialize(d).map_err(|_| {
            de::Error::custom("expected an event type glob like \"fs.*\" or a list of them")
        })? {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        };
        if items.is_empty() {
            return Err(de::Error::custom("expected at least one event type glob"));
        }
        items
            .iter()
            .map(|s| s.parse().map_err(de::Error::custom))
            .collect::<Result<_, _>>()
            .map(TypeGlobs)
    }
}

/// A permission grant: `"<type glob>"` or `"<type glob>:<path glob>"`, for example
/// `"calendar.*"` or `"fs.*:~/University/**"`. The path part is kept as written (with `~`);
/// expansion happens when routing config is compiled, with the home directory as a parameter.
#[derive(Clone, Debug, PartialEq, Eq, Hash, TS)]
#[ts(type = "string")]
pub struct Scope {
    pub types: TypeGlob,
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    #[error("invalid scope {0:?}: expected \"<type glob>\" or \"<type glob>:<path glob>\"")]
    Syntax(String),
    #[error("invalid scope {scope:?}: {source}")]
    Pattern { scope: String, source: PatternError },
}

impl FromStr for Scope {
    type Err = ScopeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (types, path) = match s.split_once(':') {
            Some((t, p)) => (t, Some(p)),
            None => (s, None),
        };
        let types =
            types.parse().map_err(|source| ScopeError::Pattern { scope: s.to_owned(), source })?;
        if let Some(p) = path {
            if !(p.starts_with('/') || p.starts_with('~')) {
                return Err(ScopeError::Syntax(s.to_owned()));
            }
        }
        Ok(Scope { types, path: path.map(str::to_owned) })
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(p) => write!(f, "{}:{p}", self.types),
            None => write!(f, "{}", self.types),
        }
    }
}
string_serde!(Scope);

/// At most `count` per `per`: `"10/hour"`, `"3/minute"`, `"5/10m"`, `"100/day"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TS)]
#[ts(type = "string")]
pub struct Rate {
    pub count: u32,
    pub per: Duration,
}

impl Rate {
    pub const MAX_COUNT: u32 = 10_000;
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "expected a rate like \"10/hour\", \"3/minute\" or \"5/10m\" (count 1..=10000, period > 0), got {0:?}"
)]
pub struct RateError(pub String);

impl FromStr for Rate {
    type Err = RateError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || RateError(s.to_owned());
        let (count, per) = s.split_once('/').ok_or_else(err)?;
        let count: u32 = count.trim().parse().map_err(|_| err())?;
        let per = match per.trim() {
            "s" | "sec" | "second" => Duration::from_secs(1),
            "m" | "min" | "minute" => Duration::from_mins(1),
            "h" | "hour" => Duration::from_hours(1),
            "d" | "day" => Duration::from_days(1),
            other => Duration::parse(other).map_err(|_| err())?,
        };
        if count == 0 || count > Rate::MAX_COUNT || per.is_zero() {
            return Err(err());
        }
        Ok(Rate { count, per })
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.count, self.per)
    }
}
string_serde!(Rate);

// ---- The `where` matcher language. ----

/// `where = { path = "~/University/**/*.pdf", exit_code = { ne = 0 } }`.
///
/// Keys address `subject`, `source`, `type`, or a path into `data` (`path` and `data.path` are
/// the same). All keys must match. A missing field never matches, except `{ exists = false }`.
pub type WhereClause = BTreeMap<String, WhereCondition>;

/// A literal in a `where` clause.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum WhereScalar {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

/// The condition for one key.
///
/// - a string is a glob (path-aware when it starts with `/` or `~`),
/// - a number or boolean is equality,
/// - a list is "any of" those,
/// - a table is a set of operators, all of which must hold.
///
/// When the field holds an array, a condition matches if any element matches (except
/// `contains`, which tests membership, and `exists`).
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(untagged)]
pub enum WhereCondition {
    Match(WhereScalar),
    AnyOf(Vec<WhereScalar>),
    Ops(WhereOps),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct WhereOps {
    /// Literal equality (strings are not globs here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<WhereScalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ne: Option<WhereScalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gt: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gte: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lt: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte: Option<f64>,
    /// Literal membership.
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub in_: Option<Vec<WhereScalar>>,
    /// Substring for strings, element equality for arrays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<WhereScalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

impl WhereOps {
    pub fn is_empty(&self) -> bool {
        *self == WhereOps::default()
    }
}

impl<'de> Deserialize<'de> for WhereCondition {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = WhereCondition;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(
                    "a glob string, a number, a boolean, a list of those, or an operator table like { ne = 0 }",
                )
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(WhereCondition::Match(WhereScalar::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(WhereCondition::Match(WhereScalar::Integer(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                i64::try_from(v)
                    .map(|v| WhereCondition::Match(WhereScalar::Integer(v)))
                    .map_err(|_| E::custom("integer out of range"))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(WhereCondition::Match(WhereScalar::Float(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(WhereCondition::Match(WhereScalar::String(v.to_owned())))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<WhereScalar>()? {
                    items.push(item);
                }
                if items.is_empty() {
                    return Err(de::Error::custom("an empty list never matches"));
                }
                Ok(WhereCondition::AnyOf(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let ops = WhereOps::deserialize(de::value::MapAccessDeserializer::new(map))?;
                if ops.is_empty() {
                    return Err(de::Error::custom(
                        "expected at least one operator: eq, ne, gt, gte, lt, lte, in, contains, glob, exists",
                    ));
                }
                Ok(WhereCondition::Ops(ops))
            }
        }
        d.deserialize_any(V)
    }
}

/// What a `where` key addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WhereField {
    Subject,
    Source,
    Type,
    Data(Vec<String>),
}

impl WhereField {
    pub fn parse(key: &str) -> Option<WhereField> {
        Some(match key {
            "subject" => WhereField::Subject,
            "source" => WhereField::Source,
            "type" => WhereField::Type,
            other => WhereField::Data(crate::template::parse_data_path(
                other.strip_prefix("data.").unwrap_or(other),
            )?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes() {
        let ok = [
            ("calendar.*", "calendar.*", None),
            ("fs.*:~/University/**", "fs.*", Some("~/University/**")),
            ("fs.file.created:/tmp/x", "fs.file.created", Some("/tmp/x")),
        ];
        for (s, types, path) in ok {
            let scope: Scope = s.parse().unwrap();
            assert_eq!(scope.types.as_str(), types);
            assert_eq!(scope.path.as_deref(), path);
            assert_eq!(scope.to_string(), s);
        }
        for bad in ["", "fs.*:", "fs.*:relative/**", "FS:/x"] {
            assert!(bad.parse::<Scope>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rates() {
        let ok = [
            ("10/hour", 10, Duration::from_hours(1)),
            ("3/minute", 3, Duration::from_mins(1)),
            ("5/10m", 5, Duration::from_mins(10)),
            ("100/day", 100, Duration::from_days(1)),
            ("1/s", 1, Duration::from_secs(1)),
        ];
        for (s, count, per) in ok {
            assert_eq!(s.parse::<Rate>().unwrap(), Rate { count, per }, "{s}");
        }
        for bad in ["10", "0/hour", "10/fortnight", "10/0s", "-1/hour", "20000/hour", "x/hour"] {
            assert!(bad.parse::<Rate>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn where_conditions_deserialize() {
        let w: WhereClause = toml::from_str(
            r#"
            path = "~/U/**/*.pdf"
            exit_code = { ne = 0 }
            success = false
            size = { gt = 5, lte = 10.5 }
            branch = ["main", "release/*"]
            "data.tags" = { contains = "urgent" }
            "#,
        )
        .unwrap();
        assert_eq!(w["path"], WhereCondition::Match(WhereScalar::String("~/U/**/*.pdf".into())));
        assert_eq!(
            w["exit_code"],
            WhereCondition::Ops(WhereOps {
                ne: Some(WhereScalar::Integer(0)),
                ..Default::default()
            })
        );
        assert_eq!(w["success"], WhereCondition::Match(WhereScalar::Bool(false)));
        assert!(matches!(&w["branch"], WhereCondition::AnyOf(v) if v.len() == 2));
        let json = serde_json::to_value(&w).unwrap();
        assert_eq!(json["exit_code"], serde_json::json!({"ne": 0}));
        let back: WhereClause = serde_json::from_value(json).unwrap();
        assert_eq!(back, w);

        let errors = [
            ("x = { nope = 1 }", "unknown field `nope`"),
            ("x = {}", "at least one operator"),
            ("x = []", "empty list"),
        ];
        for (src, want) in errors {
            let err = toml::from_str::<WhereClause>(src).unwrap_err().to_string();
            assert!(err.contains(want), "{src}: {err}");
        }
    }

    #[test]
    fn where_fields() {
        assert_eq!(WhereField::parse("subject"), Some(WhereField::Subject));
        assert_eq!(WhereField::parse("path"), Some(WhereField::Data(vec!["path".into()])));
        assert_eq!(
            WhereField::parse("data.subject"),
            Some(WhereField::Data(vec!["subject".into()]))
        );
        assert_eq!(WhereField::parse("a..b"), None);
    }
}
