//! Fails when `apps/web/src/contracts/index.ts` is stale. Regenerate with
//! `PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript`.

#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::path::PathBuf;

#[test]
fn typescript_bindings_are_current() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/web/src/contracts/index.ts");
    let generated = pulse_contracts::typescript::typescript_bindings();
    if std::env::var_os("PULSE_UPDATE_TS").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        current == generated,
        "{} is stale. Regenerate with: PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript",
        path.display()
    );
}

/// The fields of the first object literal in a TypeScript declaration, with whether each is
/// optional (`name?:`) and its type text.
fn object_fields(decl: &str) -> Vec<(String, bool, String)> {
    let mut decl = decl.to_owned();
    while let Some(start) = decl.find("/**") {
        let end = decl[start..].find("*/").map_or(decl.len(), |e| start + e + 2);
        decl.replace_range(start..end, "");
    }
    let body = {
        let open = decl.find('{').expect("an object type");
        let mut depth = 0;
        let mut close = open;
        for (i, c) in decl[open..].char_indices() {
            match c {
                '{' | '(' | '<' | '[' => depth += 1,
                '}' | ')' | '>' | ']' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                close = open + i;
                break;
            }
        }
        decl[open + 1..close].to_owned()
    };
    let mut fields = Vec::new();
    let (mut depth, mut start) = (0, 0);
    let bytes: Vec<char> = body.chars().collect();
    let mut parts = Vec::new();
    for (i, c) in bytes.iter().enumerate() {
        match c {
            '{' | '(' | '<' | '[' => depth += 1,
            '}' | ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(bytes[start..i].iter().collect::<String>());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(bytes[start..].iter().collect());
    for part in parts {
        let part = part.trim();
        let Some((name, ty)) = part.split_once(':') else { continue };
        let name = name.trim().trim_matches('"');
        let (name, optional) = match name.strip_suffix('?') {
            Some(n) => (n, true),
            None => (name, false),
        };
        fields.push((name.to_owned(), optional, ty.trim().to_owned()));
    }
    fields
}

/// A value as the daemon sends it: every field the TypeScript type requires is present, and
/// every `null` it sends is allowed by the type.
fn assert_wire_matches<T>(minimal: serde_json::Value)
where
    T: ts_rs::TS + serde::Serialize + serde::de::DeserializeOwned,
{
    let cfg = ts_rs::Config::new().with_large_int("number");
    let decl = T::decl(&cfg);
    let value: T = serde_json::from_value(minimal).unwrap();
    let wire = serde_json::to_value(&value).unwrap();
    let wire = wire.as_object().unwrap();
    for (name, optional, ty) in object_fields(&decl) {
        match wire.get(&name) {
            None => assert!(
                optional,
                "{}: `{name}` is omitted on the wire but required in TypeScript",
                T::ident(&cfg)
            ),
            Some(serde_json::Value::Null) => {
                assert!(
                    ty.contains("null"),
                    "{}: `{name}` is null on the wire but typed `{ty}`",
                    T::ident(&cfg)
                )
            }
            Some(_) => {}
        }
    }
}

#[test]
fn generated_types_tell_the_truth_about_omitted_defaults() {
    use pulse_contracts::config::{
        AgentConfig, ClaudeCodeSettings, CodexSettings, FsSourceSettings, RuleConfig,
    };
    use pulse_contracts::rpc::{EventPage, RuleTestResult, RunOutputPage};
    use serde_json::json;

    let rule = json!({ "name": "r", "on": "fs.file.created", "run": { "agent": "a" } });
    assert_wire_matches::<RuleConfig>(rule);
    let claude = json!({ "name": "a", "adapter": "claude-code" });
    assert_wire_matches::<AgentConfig>(claude.clone());
    assert_wire_matches::<ClaudeCodeSettings>(json!({}));
    assert_wire_matches::<CodexSettings>(json!({}));
    assert_wire_matches::<FsSourceSettings>(json!({ "path": "~/x" }));
    assert_wire_matches::<EventPage>(json!({ "events": [], "next_cursor": null }));
    assert_wire_matches::<RuleTestResult>(json!({ "matched": false, "decision": null }));
    assert_wire_matches::<RunOutputPage>(
        json!({ "chunks": [], "next_after": null, "stats": { "bytes": 0, "dropped_bytes": 0 }, "done": true }),
    );
}

#[test]
fn optional_params_are_optional_in_typescript() {
    let cfg = ts_rs::Config::new().with_large_int("number");
    for decl in [
        <pulse_contracts::rpc::RunOutputParams as ts_rs::TS>::decl(&cfg),
        <pulse_contracts::rpc::RunOutputSubscribeParams as ts_rs::TS>::decl(&cfg),
    ] {
        let after = object_fields(&decl).into_iter().find(|f| f.0 == "after").unwrap();
        assert!(after.1, "`after` is `Option` in Rust: {decl}");
    }
}
