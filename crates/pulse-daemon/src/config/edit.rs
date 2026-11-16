//! Format-preserving write-back: upsert or remove one `[[rule]]`, `[[agent]]` or `[[source]]`
//! in a config file's text.
//!
//! An update touches only the keys whose *meaning* changed. Old and new are compared after a
//! round trip through the typed definition, so `on = "fs.*"` and `on = ["fs.*"]` are the same,
//! and a key left out because it has its default stays left out. Unchanged keys keep their
//! text, comments and position; a changed value keeps its key's comments and its trailing
//! comment. New definitions are appended without keys that only restate defaults.

use serde::Serialize;
use serde::de::DeserializeOwned;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DefKind {
    Rule,
    Agent,
    Source,
}

impl DefKind {
    pub(crate) fn array(self) -> &'static str {
        match self {
            DefKind::Rule => "rule",
            DefKind::Agent => "agent",
            DefKind::Source => "source",
        }
    }

    /// Keys written first and last in a new definition; the rest keep serializer order.
    fn layout(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            DefKind::Rule => (&["name", "description", "on", "where"], &["run"]),
            DefKind::Agent => (
                &["name", "adapter"],
                &["allow", "cwd", "timeout", "max_concurrent", "max_queued", "env"],
            ),
            DefKind::Source => (&["name", "kind"], &["enabled"]),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum EditError {
    #[error("the file is not valid TOML, so Pulse cannot edit it: {0}")]
    Parse(String),
    #[error("`{0}` must be written as [[{0}]] tables for Pulse to edit it")]
    Shape(&'static str),
    #[error("could not serialize the definition: {0}")]
    Serialize(String),
    #[error("not found")]
    NotFound,
}

fn ser(e: impl std::fmt::Display) -> EditError {
    EditError::Serialize(e.to_string())
}

/// The table's meaning, normalized through `T` (defaults filled in, canonical forms).
fn normalized<T: Serialize + DeserializeOwned>(table: &Table) -> Option<toml::Table> {
    let mut doc = DocumentMut::new();
    for (k, v) in table.iter() {
        doc.insert(k, v.clone());
    }
    let parsed: toml::Table = toml::from_str(&doc.to_string()).ok()?;
    let typed: T = parsed.try_into().ok()?;
    toml::Table::try_from(&typed).ok()
}

fn inline(item: Item) -> Option<Value> {
    match item {
        Item::Value(v) => Some(v),
        Item::Table(t) => {
            let mut t = t.into_inline_table();
            toml_edit::InlineTable::fmt(&mut t);
            Some(Value::InlineTable(t))
        }
        Item::ArrayOfTables(a) => Some(Value::Array(a.into_array())),
        Item::None => None,
    }
}

/// The new definition's keys, formatted, in layout order.
fn formatted<T: Serialize>(kind: DefKind, def: &T) -> Result<Vec<(String, Value)>, EditError> {
    let doc: DocumentMut = toml::to_string(def).map_err(ser)?.parse().map_err(ser)?;
    let mut items: Vec<(String, Value)> = doc
        .as_table()
        .iter()
        .filter_map(|(k, v)| {
            let mut v = inline(v.clone())?;
            v.decor_mut().clear();
            Some((k.to_owned(), v))
        })
        .collect();
    let (first, last) = kind.layout();
    let rank = |k: &str| {
        first
            .iter()
            .position(|f| *f == k)
            .map(|i| (0, i))
            .or_else(|| last.iter().position(|l| *l == k).map(|i| (2, i)))
            .unwrap_or((1, 0))
    };
    items.sort_by_key(|(k, _)| rank(k));
    Ok(items)
}

fn definitions(doc: &mut DocumentMut, kind: DefKind) -> Result<&mut ArrayOfTables, EditError> {
    let item = doc.entry(kind.array()).or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    item.as_array_of_tables_mut().ok_or(EditError::Shape(kind.array()))
}

/// Replaces the definition named `name` (the bare name, as written in the file) with `def`, or
/// appends it.
pub(crate) fn upsert<T>(text: &str, kind: DefKind, name: &str, def: &T) -> Result<String, EditError>
where
    T: Serialize + DeserializeOwned + PartialEq,
{
    let mut doc: DocumentMut = text.parse().map_err(|e| EditError::Parse(format!("{e}")))?;
    let new: toml::Table = toml::Table::try_from(def).map_err(ser)?;
    let values = formatted(kind, def)?;
    let tables = definitions(&mut doc, kind)?;
    let existing = tables.iter_mut().find(|t| t.get("name").and_then(Item::as_str) == Some(name));
    match existing {
        Some(table) => {
            let old = normalized::<T>(table);
            let same = |k: &str| old.as_ref().is_some_and(|o| o.get(k) == new.get(k));
            let present: Vec<String> = table.iter().map(|(k, _)| k.to_owned()).collect();
            for k in present {
                if !same(&k) && !new.contains_key(&k) {
                    table.remove(&k);
                }
            }
            for (k, mut value) in values {
                if same(&k) {
                    continue;
                }
                // Back to its default: drop the line, unless someone commented on it.
                let commented = table.key(&k).is_some_and(|key| has_comment(key.leaf_decor()))
                    || table
                        .get(&k)
                        .and_then(Item::as_value)
                        .is_some_and(|v| has_comment(v.decor()));
                if table.contains_key(&k) && !commented && redundant(&new, &k, def) {
                    table.remove(&k);
                    continue;
                }
                match table.get_mut(&k) {
                    Some(Item::Value(old_value)) => {
                        *value.decor_mut() = old_value.decor().clone();
                        *old_value = value;
                    }
                    Some(slot) => *slot = Item::Value(value),
                    None => {
                        table.insert(&k, Item::Value(value.decorated(" ", "")));
                    }
                }
            }
        }
        None => {
            let mut table = Table::new();
            for (k, value) in without_defaults(def, values)? {
                table.insert(&k, Item::Value(value.decorated(" ", "")));
            }
            tables.push(table);
        }
    }
    Ok(doc.to_string())
}

/// Drops keys whose absence deserializes to the same definition.
fn without_defaults<T>(
    def: &T,
    values: Vec<(String, Value)>,
) -> Result<Vec<(String, Value)>, EditError>
where
    T: Serialize + DeserializeOwned + PartialEq,
{
    let mut table = toml::Table::try_from(def).map_err(ser)?;
    let mut keep = Vec::new();
    for (k, v) in values.into_iter().rev() {
        if redundant(&table, &k, def) {
            table.remove(&k);
        } else {
            keep.push((k, v));
        }
    }
    keep.reverse();
    Ok(keep)
}

/// Whether leaving `key` out of `table` still deserializes to `def` (it holds a default).
fn redundant<T>(table: &toml::Table, key: &str, def: &T) -> bool
where
    T: DeserializeOwned + PartialEq,
{
    let mut without = table.clone();
    without.remove(key);
    key != "name" && without.try_into::<T>().is_ok_and(|t| t == *def)
}

fn has_comment(decor: &toml_edit::Decor) -> bool {
    let has = |r: Option<&toml_edit::RawString>| {
        r.and_then(|r| r.as_str()).is_some_and(|s| s.contains('#'))
    };
    has(decor.prefix()) || has(decor.suffix())
}

/// Removes the definition named `name`.
pub(crate) fn remove(text: &str, kind: DefKind, name: &str) -> Result<String, EditError> {
    let mut doc: DocumentMut = text.parse().map_err(|e| EditError::Parse(format!("{e}")))?;
    let tables = definitions(&mut doc, kind)?;
    let index = tables
        .iter()
        .position(|t| t.get("name").and_then(Item::as_str) == Some(name))
        .ok_or(EditError::NotFound)?;
    tables.remove(index);
    if tables.is_empty() {
        doc.remove(kind.array());
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use pulse_contracts::config::{AgentConfig, RuleConfig};

    use super::*;

    const FILE: &str = r#"# My Pulse config.

[[agent]]
name = "notify"     # desktop notifications
adapter = "command"
command = ["notify-send", "--", "{{subject}}"]
allow = ["fs.*:~/U/**"]

# The study rule.
[[rule]]
name = "new-lecture"
on = "fs.file.created"            # a type glob
where = { path = "~/U/**/*.pdf" }
# Wait for the file to settle.
debounce = "10s"
run = { agent = "notify" }

[[rule]]
name = "other"
on = ["git.*"]
run = { agent = "notify" }
"#;

    fn rule(text: &str, name: &str) -> RuleConfig {
        let file: pulse_contracts::config::ConfigFile = toml::from_str(text).unwrap();
        file.rules.into_iter().find(|r| r.name == name).unwrap()
    }

    #[test]
    fn changing_one_field_touches_one_line() {
        let mut r = rule(FILE, "new-lecture");
        r.enabled = false;
        r.debounce = Some(pulse_contracts::Duration::from_secs(30));
        let out = upsert(FILE, DefKind::Rule, "new-lecture", &r).unwrap();
        let want = FILE.replace("debounce = \"10s\"", "debounce = \"30s\"").replace(
            "run = { agent = \"notify\" }\n\n[[rule]]",
            "run = { agent = \"notify\" }\nenabled = false\n\n[[rule]]",
        );
        assert_eq!(out, want);
        assert_eq!(rule(&out, "new-lecture"), r);
        // Undoing both changes restores the file exactly: the default is not spelled out.
        r.enabled = true;
        r.debounce = Some(pulse_contracts::Duration::from_secs(10));
        assert_eq!(upsert(&out, DefKind::Rule, "new-lecture", &r).unwrap(), FILE);
        // Writing back the same definition is a no-op, even where the file uses short forms.
        let same = rule(FILE, "other");
        assert_eq!(upsert(FILE, DefKind::Rule, "other", &same).unwrap(), FILE);
    }

    #[test]
    fn changed_values_keep_their_comments_and_removed_keys_go() {
        let mut r = rule(FILE, "new-lecture");
        r.on = toml::from_str::<OnOnly>("on = \"fs.file.*\"").unwrap().on;
        r.debounce = None;
        let out = upsert(FILE, DefKind::Rule, "new-lecture", &r).unwrap();
        assert!(out.contains("on = [\"fs.file.*\"]            # a type glob\n"), "{out}");
        assert!(!out.contains("debounce"), "{out}");
        assert!(out.contains("# The study rule.\n[[rule]]\nname = \"new-lecture\""), "{out}");
        assert_eq!(rule(&out, "new-lecture"), r);
    }

    #[derive(serde::Deserialize)]
    struct OnOnly {
        on: pulse_contracts::config::TypeGlobs,
    }

    #[test]
    fn appending_and_removing() {
        let mut r = rule(FILE, "other");
        r.name = "third".into();
        r.supervised = true;
        let out = upsert(FILE, DefKind::Rule, "third", &r).unwrap();
        assert!(out.starts_with(FILE), "existing text is untouched");
        let added = &out[FILE.len()..];
        assert_eq!(
            added,
            "\n[[rule]]\nname = \"third\"\non = [\"git.*\"]\nsupervised = true\nrun = { agent = \"notify\" }\n"
        );
        assert_eq!(rule(&out, "third"), r);

        let out = remove(&out, DefKind::Rule, "new-lecture").unwrap();
        assert!(!out.contains("new-lecture") && !out.contains("The study rule"), "{out}");
        assert!(out.contains("# My Pulse config."));
        assert!(matches!(remove(&out, DefKind::Rule, "nope"), Err(EditError::NotFound)));

        let agent: AgentConfig = {
            let file: pulse_contracts::config::ConfigFile = toml::from_str(FILE).unwrap();
            file.agents[0].clone()
        };
        let out = upsert("", DefKind::Agent, "notify", &agent).unwrap();
        assert_eq!(
            out,
            "[[agent]]\nname = \"notify\"\nadapter = \"command\"\ncommand = [\"notify-send\", \"--\", \"{{subject}}\"]\nallow = [\"fs.*:~/U/**\"]\n"
        );
        assert!(matches!(
            upsert("rule = 3", DefKind::Rule, "x", &r),
            Err(EditError::Shape("rule"))
        ));
        assert!(matches!(upsert("[[rule]", DefKind::Rule, "x", &r), Err(EditError::Parse(_))));
    }
}
