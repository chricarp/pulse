//! Format-preserving write-back: upsert or remove one `[[rule]]`, `[[agent]]` or `[[source]]`
//! in a config file's text.
//!
//! An update touches only the keys whose *meaning* changed. Old and new are compared after a
//! round trip through the typed definition, so `on = "fs.*"` and `on = ["fs.*"]` are the same,
//! and a key left out because it has its default stays left out. Unchanged keys keep their
//! text, comments and position; a changed value keeps its key's comments and its trailing
//! comment. New definitions are appended without keys that only restate defaults.
//!
//! Comments are kept where the person wrote them. `toml_edit` keeps the comments and blank lines
//! before a table header as that table's prefix, and those at the end of the file as the
//! document's trailer, so the first definition's prefix holds the file's header. A prefix is
//! read as three parts: lines directly below the previous definition (up to the first blank
//! line) belong to it; lines directly above the header (after the last blank line) belong to
//! this definition; what lies between is free-standing. Removing a definition drops its own
//! parts and moves the free-standing ones to whatever follows. A new definition goes at the
//! very end, after any comments there.

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

/// The table as TOML text of its own.
fn table_text(table: &Table) -> String {
    let mut doc = DocumentMut::new();
    for (k, v) in table.iter() {
        doc.insert(k, v.clone());
    }
    doc.to_string()
}

/// The table read as `T`, the way a whole file is read.
fn typed<T: DeserializeOwned>(table: &Table) -> Option<T> {
    toml::from_str(&table_text(table)).ok()
}

/// The table's meaning, normalized through `T` (defaults filled in, canonical forms).
fn normalized<T: Serialize + DeserializeOwned>(table: &Table) -> Option<toml::Table> {
    toml::Table::try_from(&typed::<T>(table)?).ok()
}

/// A definition as it is in a file's text.
#[derive(Debug)]
pub(crate) struct Found<T> {
    /// [`super::revision`] of the definition; for one that does not read as `T`, a hash of its
    /// text (it was never handed out, so no client can expect it by accident).
    pub revision: String,
    /// `None` when it does not read as a valid `T`.
    pub def: Option<T>,
}

/// The definition named `name` (bare, as written) in `text`, if there is one.
pub(crate) fn find<T>(text: &str, kind: DefKind, name: &str) -> Result<Option<Found<T>>, EditError>
where
    T: Serialize + DeserializeOwned,
{
    let doc: DocumentMut = text.parse().map_err(|e| EditError::Parse(format!("{e}")))?;
    let Some(item) = doc.get(kind.array()) else { return Ok(None) };
    let tables = item.as_array_of_tables().ok_or(EditError::Shape(kind.array()))?;
    let Some(table) = tables.iter().find(|t| t.get("name").and_then(Item::as_str) == Some(name))
    else {
        return Ok(None);
    };
    Ok(Some(match typed::<T>(table) {
        Some(def) => Found { revision: super::revision(&def), def: Some(def) },
        None => Found { revision: super::hash(table_text(table).as_bytes()), def: None },
    }))
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
            // The trailer would be printed after the new table; it stays above it instead.
            let trailer = raw(Some(doc.trailing()));
            if !trailer.is_empty() {
                let sep =
                    if lines(&trailer).last().is_some_and(|l| is_blank(l)) { "" } else { "\n" };
                doc.set_trailing("");
                if let Some(t) = definitions(&mut doc, kind)?.iter_mut().last() {
                    t.decor_mut().set_prefix(format!("{trailer}{sep}"));
                }
            }
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

/// Removes the definition named `name`, with the comments directly above and below it and the
/// blank line that separated it from what follows. Free-standing comments in its prefix (the
/// file's header, for the first definition) move to whatever follows it.
pub(crate) fn remove(text: &str, kind: DefKind, name: &str) -> Result<String, EditError> {
    let mut doc: DocumentMut = text.parse().map_err(|e| EditError::Parse(format!("{e}")))?;
    let tables = definitions(&mut doc, kind)?;
    let index = tables
        .iter()
        .position(|t| t.get("name").and_then(Item::as_str) == Some(name))
        .ok_or(EditError::NotFound)?;
    let removed = tables.remove(index);
    if tables.is_empty() {
        doc.remove(kind.array());
    }
    let (free, _own) = split_above(&raw(removed.decor().prefix()));
    // Its own sub-tables (`[rule.run]`) follow it; what follows them is the next item.
    let mut last = removed.position();
    visit_tables(&removed, &mut |t| last = last.max(t.position()));
    let mut next_pos = None;
    if let Some(last) = last {
        visit_tables(doc.as_table(), &mut |t| {
            if let Some(p) = t.position().filter(|p| *p > last) {
                next_pos = Some(next_pos.map_or(p, |n: isize| n.min(p)));
            }
        });
    }
    let mut next = None;
    if let Some(pos) = next_pos {
        visit_tables_mut(doc.as_table_mut(), &mut |t| {
            if next.is_none() && t.position() == Some(pos) {
                let prefix = join(&free, below(&raw(t.decor().prefix())));
                t.decor_mut().set_prefix(prefix);
                next = Some(pos);
            }
        });
    }
    if next.is_none() {
        let trailer = join(&free, below(&raw(Some(doc.trailing()))));
        let end = lines(&trailer).iter().rposition(|l| !is_blank(l)).map_or(0, |i| i + 1);
        doc.set_trailing(lines(&trailer)[..end].concat());
    }
    Ok(doc.to_string())
}

fn raw(r: Option<&toml_edit::RawString>) -> String {
    r.and_then(|r| r.as_str()).unwrap_or_default().to_owned()
}

fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// A prefix split at its last blank line: free-standing lines (ending with that blank line),
/// and the lines directly above the header.
fn split_above(prefix: &str) -> (String, String) {
    let lines = lines(prefix);
    let at = lines.iter().rposition(|l| is_blank(l)).map_or(0, |i| i + 1);
    (lines[..at].concat(), lines[at..].concat())
}

/// A prefix (or the trailer) without the lines directly below the previous item, which belong
/// to it. Lines with no blank line after them are the next header's, not the previous item's.
fn below(prefix: &str) -> String {
    let lines = lines(prefix);
    match lines.iter().position(|l| is_blank(l)) {
        Some(i) => lines[i..].concat(),
        None => prefix.to_owned(),
    }
}

/// `free` (empty, or ending with a blank line) followed by `rest`, without the blank lines that
/// separated the removed definition from `rest`.
fn join(free: &str, rest: String) -> String {
    let lines = lines(&rest);
    let start = lines.iter().position(|l| !is_blank(l)).unwrap_or(lines.len());
    format!("{free}{}", lines[start..].concat())
}

/// Every table below `table` with a header of its own, in no particular order.
fn visit_tables(table: &Table, f: &mut dyn FnMut(&Table)) {
    for (_, item) in table.iter() {
        match item {
            Item::Table(t) => {
                f(t);
                visit_tables(t, f);
            }
            Item::ArrayOfTables(a) => {
                for t in a.iter() {
                    f(t);
                    visit_tables(t, f);
                }
            }
            Item::None | Item::Value(_) => {}
        }
    }
}

fn visit_tables_mut(table: &mut Table, f: &mut dyn FnMut(&mut Table)) {
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(t) => {
                f(t);
                visit_tables_mut(t, f);
            }
            Item::ArrayOfTables(a) => {
                for t in a.iter_mut() {
                    f(t);
                    visit_tables_mut(t, f);
                }
            }
            Item::None | Item::Value(_) => {}
        }
    }
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

    /// A file's header, free-standing comments between definitions and at the end, comments
    /// attached above and below definitions, and definitions of two kinds interleaved.
    const HEAD: &str = "# Header 1\n# Header 2\n\n# Free before first.\n\n";
    const FIRST: &str = "# attached to first\n[[rule]]\nname = \"first\"\non = \"a.*\"\nrun = { agent = \"n\" }\n# below first\n\n";
    const BETWEEN: &str = "# Free between.\n\n";
    const SECOND: &str = "# attached to second\n[[agent]]\nname = \"n\"\nadapter = \"command\"\ncommand = [\"true\"]\nallow = [\"*\"]\n\n";
    const THIRD: &str =
        "[[rule]]\nname = \"third\"\non = \"b.*\"\nrun = { agent = \"n\" }\n# below third\n";
    const END: &str = "\n# Free at the end.\n";

    #[test]
    fn removing_drops_the_definition_and_its_own_comments_only() {
        let file = [HEAD, FIRST, BETWEEN, SECOND, THIRD, END].concat();
        // (kind, name, the exact text that goes: the definition, the comments directly above and
        // below it, and one blank line that separated it)
        let cases = [
            (DefKind::Rule, "first", FIRST.to_owned()),
            (DefKind::Agent, "n", SECOND.to_owned()),
            (DefKind::Rule, "third", format!("{THIRD}\n")),
        ];
        for (kind, name, cut) in cases {
            let out = remove(&file, kind, name).unwrap();
            assert_eq!(out, file.replacen(&cut, "", 1), "removing {name}");
        }
        // The only definition in the file.
        let only = [HEAD, FIRST, END].concat();
        assert_eq!(
            remove(&only, DefKind::Rule, "first").unwrap(),
            [HEAD, "# Free at the end.\n"].concat()
        );
        // Without comments, and with nothing after it.
        let bare = "[[rule]]\nname = \"a\"\non = \"x\"\nrun = { agent = \"n\" }\n\n[[rule]]\nname = \"b\"\non = \"x\"\nrun = { agent = \"n\" }\n";
        let (a, b) = bare.split_at(bare.find("\n\n").unwrap() + 2);
        assert_eq!(remove(bare, DefKind::Rule, "a").unwrap(), b);
        assert_eq!(remove(bare, DefKind::Rule, "b").unwrap(), &a[..a.len() - 1]);
        assert_eq!(remove(b, DefKind::Rule, "b").unwrap(), "");
    }

    #[test]
    fn removing_the_first_definition_keeps_the_files_comments() {
        // The template as `pulse init --template` writes it: its explanation (and, in a starter
        // file, the starter's examples) is the first definition's decor in `toml_edit`.
        let file = include_str!("../../../../templates/lecture-notes.toml");
        let out = remove(file, DefKind::Source, "university").unwrap();
        let comments = |t: &str| t.lines().filter(|l| l.starts_with('#')).count();
        assert_eq!(comments(&out), comments(file), "{out}");
        let source =
            "[[source]]\nname = \"university\"\nkind = \"fs\"\npath = \"~/University\"\n\n";
        assert_eq!(out, file.replacen(source, "", 1));
        // The last definition, with a sub-table and a comment among its keys.
        let rule = &file[file.find("\n[[rule]]").unwrap()..];
        assert_eq!(
            remove(file, DefKind::Rule, "lecture-notes").unwrap(),
            file.replacen(rule, "", 1)
        );
    }

    #[test]
    fn appending_goes_after_everything_already_in_the_file() {
        let r = rule(&[HEAD, FIRST, BETWEEN, SECOND, THIRD].concat(), "third");
        let new = |name: &str| RuleConfig { name: name.into(), ..r.clone() };
        let added = |name: &str| {
            format!("[[rule]]\nname = \"{name}\"\non = [\"b.*\"]\nrun = {{ agent = \"n\" }}\n")
        };
        // A comment directly below the last definition stays with it, and free-standing comments
        // at the end stay where they were written: the new definition goes after both.
        for file in [
            [HEAD, FIRST, BETWEEN, SECOND, THIRD, END].concat(),
            [HEAD, FIRST, BETWEEN, SECOND, THIRD].concat(),
            [FIRST, "\n# Ends with a blank line.\n\n"].concat(),
            // Only comments (the starter file).
            HEAD.trim_end().to_owned() + "\n",
        ] {
            let out = upsert(&file, DefKind::Rule, "new", &new("new")).unwrap();
            let sep = if file.ends_with("\n\n") { "" } else { "\n" };
            assert_eq!(out, format!("{file}{sep}{}", added("new")), "{file}");
            let twice = upsert(&out, DefKind::Rule, "newer", &new("newer")).unwrap();
            assert_eq!(twice, format!("{out}\n{}", added("newer")));
        }
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
