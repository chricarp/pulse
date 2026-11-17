//! The `where` matcher language, compiled from `pulse_contracts::config::WhereClause`.
//!
//! Semantics (also documented on the contract types):
//! - keys address `subject`, `source`, `type`, or a path into `data` (`path` ≡ `data.path`;
//!   numeric segments index arrays);
//! - every key must match; a missing or `null` field never matches, except `{ exists = false }`;
//! - a string is a glob, path-aware when it starts with `/` or `~`; numbers compare numerically;
//!   booleans by equality; a list is "any of";
//! - when the field holds an array, a condition holds if any element satisfies it (`contains`
//!   tests membership; `exists` tests the field);
//! - `source` patterns without `://` match the source *name* (`university`), with `://` the URI.

use std::path::Path;

use pulse_contracts::Event;
use pulse_contracts::config::{WhereClause, WhereCondition, WhereField, WhereOps, WhereScalar};
use pulse_contracts::pattern::{
    Glob, PatternError, looks_like_path, parse_path_glob, parse_text_glob,
};
use serde_json::Value;

use crate::glob;

#[derive(Clone, Debug)]
pub struct CompiledWhere {
    keys: Vec<(WhereField, Cond)>,
}

#[derive(Clone, Debug)]
enum Item {
    Glob(Glob),
    Num(f64),
    Bool(bool),
}

#[derive(Clone, Debug)]
enum Literal {
    Str(String),
    Num(f64),
    Bool(bool),
}

#[derive(Clone, Debug)]
enum Op {
    Eq(Literal),
    Ne(Literal),
    Gt(f64),
    Gte(f64),
    Lt(f64),
    Lte(f64),
    In(Vec<Literal>),
    Contains(Literal),
    Glob(Glob),
    Exists(bool),
}

#[derive(Clone, Debug)]
enum Cond {
    AnyOf(Vec<Item>),
    All(Vec<Op>),
}

#[derive(Debug, thiserror::Error)]
pub enum MatcherError {
    #[error("invalid where key {0:?}")]
    Key(String),
    #[error("where.{key}: {source}")]
    Pattern { key: String, source: PatternError },
}

fn compile_glob(s: &str, home: &Path) -> Result<Glob, PatternError> {
    if looks_like_path(s) { parse_path_glob(s, home) } else { parse_text_glob(s) }
}

fn literal(s: &WhereScalar) -> Literal {
    match s {
        WhereScalar::String(v) => Literal::Str(v.clone()),
        WhereScalar::Integer(v) => Literal::Num(*v as f64),
        WhereScalar::Float(v) => Literal::Num(*v),
        WhereScalar::Bool(v) => Literal::Bool(*v),
    }
}

impl CompiledWhere {
    pub fn always() -> Self {
        CompiledWhere { keys: Vec::new() }
    }

    pub fn compile(clause: &WhereClause, home: &Path) -> Result<Self, MatcherError> {
        let mut keys = Vec::new();
        for (key, cond) in clause {
            let field = WhereField::parse(key).ok_or_else(|| MatcherError::Key(key.clone()))?;
            let pat = |s: &str| {
                compile_glob(s, home)
                    .map_err(|source| MatcherError::Pattern { key: key.clone(), source })
            };
            let item = |s: &WhereScalar| -> Result<Item, MatcherError> {
                Ok(match s {
                    WhereScalar::String(v) => Item::Glob(pat(v)?),
                    WhereScalar::Integer(v) => Item::Num(*v as f64),
                    WhereScalar::Float(v) => Item::Num(*v),
                    WhereScalar::Bool(v) => Item::Bool(*v),
                })
            };
            let cond = match cond {
                WhereCondition::Match(s) => Cond::AnyOf(vec![item(s)?]),
                WhereCondition::AnyOf(list) => {
                    Cond::AnyOf(list.iter().map(item).collect::<Result<_, _>>()?)
                }
                WhereCondition::Ops(ops) => Cond::All(compile_ops(ops, pat)?),
            };
            keys.push((field, cond));
        }
        Ok(CompiledWhere { keys })
    }

    pub fn matches(&self, event: &Event) -> bool {
        self.keys.iter().all(|(field, cond)| {
            let value = lookup(event, field);
            let value = value.as_ref().filter(|v| !v.is_null());
            cond_holds(cond, value, field)
        })
    }
}

fn compile_ops(
    ops: &WhereOps,
    pat: impl Fn(&str) -> Result<Glob, MatcherError>,
) -> Result<Vec<Op>, MatcherError> {
    let mut out = Vec::new();
    if let Some(v) = &ops.eq {
        out.push(Op::Eq(literal(v)));
    }
    if let Some(v) = &ops.ne {
        out.push(Op::Ne(literal(v)));
    }
    out.extend(ops.gt.map(Op::Gt));
    out.extend(ops.gte.map(Op::Gte));
    out.extend(ops.lt.map(Op::Lt));
    out.extend(ops.lte.map(Op::Lte));
    if let Some(v) = &ops.in_ {
        out.push(Op::In(v.iter().map(literal).collect()));
    }
    if let Some(v) = &ops.contains {
        out.push(Op::Contains(literal(v)));
    }
    if let Some(g) = &ops.glob {
        out.push(Op::Glob(pat(g)?));
    }
    out.extend(ops.exists.map(Op::Exists));
    Ok(out)
}

fn lookup(event: &Event, field: &WhereField) -> Option<Value> {
    match field {
        WhereField::Subject => event.subject.clone().map(Value::String),
        WhereField::Source => Some(Value::String(event.source.clone())),
        WhereField::Type => Some(Value::String(event.type_.to_string())),
        WhereField::Data(path) => {
            let mut v = &event.data;
            for seg in path {
                v = match v {
                    Value::Object(m) => m.get(seg)?,
                    Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
                    _ => return None,
                };
            }
            Some(v.clone())
        }
    }
}

/// Applies `f` to the value, or to each element if it is an array.
fn any_scalar(v: &Value, f: &dyn Fn(&Value) -> bool) -> bool {
    match v {
        Value::Array(items) => items.iter().any(f),
        other => f(other),
    }
}

fn text_for_glob<'a>(v: &'a Value, field: &WhereField, g: &Glob) -> Option<&'a str> {
    let s = v.as_str()?;
    if *field == WhereField::Source && !g.pattern.contains("://") {
        // `pulse://<kind>/<name>`: match the name (which may itself be `project/name`).
        return Some(
            s.strip_prefix("pulse://").and_then(|r| r.split_once('/')).map_or(s, |(_, n)| n),
        );
    }
    Some(s)
}

fn literal_eq(l: &Literal, v: &Value) -> bool {
    match (l, v) {
        (Literal::Str(a), Value::String(b)) => a == b,
        (Literal::Num(a), Value::Number(b)) => b.as_f64() == Some(*a),
        (Literal::Bool(a), Value::Bool(b)) => a == b,
        _ => false,
    }
}

fn num_cmp(v: &Value, f: impl Fn(f64) -> bool) -> bool {
    v.as_f64().is_some_and(f)
}

fn cond_holds(cond: &Cond, value: Option<&Value>, field: &WhereField) -> bool {
    match cond {
        Cond::AnyOf(items) => {
            let Some(v) = value else { return false };
            items.iter().any(|item| {
                any_scalar(v, &|x| match item {
                    Item::Glob(g) => {
                        text_for_glob(x, field, g).is_some_and(|s| glob::matches(g, s))
                    }
                    Item::Num(n) => x.as_f64() == Some(*n) && x.is_number(),
                    Item::Bool(b) => x.as_bool() == Some(*b),
                })
            })
        }
        Cond::All(ops) => ops.iter().all(|op| {
            if let Op::Exists(want) = op {
                return value.is_some() == *want;
            }
            let Some(v) = value else { return false };
            match op {
                Op::Eq(l) => any_scalar(v, &|x| literal_eq(l, x)),
                Op::Ne(l) => !any_scalar(v, &|x| literal_eq(l, x)),
                Op::Gt(n) => any_scalar(v, &|x| num_cmp(x, |y| y > *n)),
                Op::Gte(n) => any_scalar(v, &|x| num_cmp(x, |y| y >= *n)),
                Op::Lt(n) => any_scalar(v, &|x| num_cmp(x, |y| y < *n)),
                Op::Lte(n) => any_scalar(v, &|x| num_cmp(x, |y| y <= *n)),
                Op::In(ls) => any_scalar(v, &|x| ls.iter().any(|l| literal_eq(l, x))),
                Op::Contains(l) => match (v, l) {
                    (Value::String(s), Literal::Str(needle)) => s.contains(needle.as_str()),
                    (Value::Array(items), l) => items.iter().any(|x| literal_eq(l, x)),
                    _ => false,
                },
                Op::Glob(g) => any_scalar(v, &|x| {
                    text_for_glob(x, field, g).is_some_and(|s| glob::matches(g, s))
                }),
                Op::Exists(_) => true,
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use pulse_contracts::ids::Ulid;
    use pulse_contracts::{EventId, EventType, Timestamp};
    use serde_json::json;

    use super::*;

    fn event(type_: &str, subject: Option<&str>, data: Value) -> Event {
        Event {
            specversion: Default::default(),
            id: EventId::from_ulid(Ulid::from_parts(1, 1)),
            type_: EventType::new(type_).unwrap(),
            source: "pulse://fs/university".into(),
            subject: subject.map(str::to_owned),
            time: Timestamp::from_millis(0),
            datacontenttype: None,
            data,
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
    fn where_table() {
        let home = Path::new("/home/me");
        let pdf = event(
            "fs.file.created",
            Some("/home/me/University/Arch/l7.pdf"),
            json!({"path": "/home/me/University/Arch/l7.pdf", "ext": "pdf", "size": 482113, "tags": ["urgent", "x"], "n": null}),
        );
        let proc = event(
            "process.exited",
            Some("cargo test"),
            json!({"exit_code": 101, "success": false, "command": ["cargo", "test"], "cwd": "/home/me/code/pulse"}),
        );
        // (where clause as TOML, event, matches)
        let cases: Vec<(&str, &Event, bool)> = vec![
            ("", &pdf, true),
            (r#"path = "~/University/**/*.pdf""#, &pdf, true),
            (r#"path = "~/University/*.pdf""#, &pdf, false),
            (r#"subject = "~/University/**""#, &pdf, true),
            (r#""data.path" = "/home/me/**""#, &pdf, true),
            (r#"ext = ["pdf", "pptx"]"#, &pdf, true),
            (r#"ext = ["doc", "pptx"]"#, &pdf, false),
            (r#"ext = "p*""#, &pdf, true),
            (r#"size = { gt = 100000 }"#, &pdf, true),
            (r#"size = { gt = 100000, lt = 200000 }"#, &pdf, false),
            (r#"size = 482113"#, &pdf, true),
            (r#"size = "482113""#, &pdf, false),
            (r#"tags = { contains = "urgent" }"#, &pdf, true),
            (r#"tags = "urg*""#, &pdf, true),
            (r#"tags = { contains = "urg" }"#, &pdf, false),
            (r#"missing = "x""#, &pdf, false),
            (r#"missing = { exists = false }"#, &pdf, true),
            (r#"missing = { ne = 1 }"#, &pdf, false),
            (r#"n = { exists = false }"#, &pdf, true),
            (r#"ext = { exists = true }"#, &pdf, true),
            (r#"source = "university""#, &pdf, true),
            (r#"source = "univ*""#, &pdf, true),
            (r#"source = "pulse://fs/university""#, &pdf, true),
            (r#"source = "pulse://git/*""#, &pdf, false),
            (r#"type = "fs.file.*""#, &pdf, true),
            (r#"exit_code = { ne = 0 }"#, &proc, true),
            (r#"exit_code = { ne = 101 }"#, &proc, false),
            (r#"exit_code = { in = [1, 101] }"#, &proc, true),
            (r#"success = false"#, &proc, true),
            (r#"success = true"#, &proc, false),
            (r#""command.0" = "cargo""#, &proc, true),
            (r#"command = { contains = "test" }"#, &proc, true),
            (r#"subject = { contains = "cargo" }"#, &proc, true),
            (r#"subject = "cargo*""#, &proc, true),
            (r#"cwd = "~/code/**""#, &proc, true),
            (r#"cwd = { eq = "~/code/pulse" }"#, &proc, false),
            (
                r#"exit_code = { ne = 0 }
                success = true"#,
                &proc,
                false,
            ),
        ];
        for (src, ev, want) in cases {
            let clause: WhereClause = toml::from_str(src).unwrap();
            let m = CompiledWhere::compile(&clause, home).unwrap();
            assert_eq!(m.matches(ev), want, "where {{ {src} }} on {}", ev.type_);
        }
    }
}
