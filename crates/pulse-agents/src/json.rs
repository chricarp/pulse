//! Helpers shared by the JSON-stream parsers: bounding provider data and summarizing it.

use pulse_contracts::{OutputKind, RunOutput};
use serde_json::{Map, Value};

use crate::Stream;

/// Strings inside `RunOutput::data` are cut to this many bytes.
pub(crate) const DATA_STRING_MAX: usize = 4 * 1024;
/// Arrays and objects inside `data` keep this many entries.
const DATA_ITEMS_MAX: usize = 100;
const DATA_DEPTH_MAX: usize = 8;
/// Tool results are previewed in `text` up to this many characters.
pub(crate) const PREVIEW_CHARS: usize = 2000;
/// One-line summaries (tool calls, statuses) are cut to this many characters.
pub(crate) const SUMMARY_CHARS: usize = 300;

/// A line the parser does not understand, passed through as it is.
pub(crate) fn raw(stream: Stream, line: &str) -> RunOutput {
    let kind = match stream {
        Stream::Stdout => OutputKind::Stdout,
        Stream::Stderr => OutputKind::Stderr,
    };
    RunOutput::new(kind, line)
}

pub(crate) fn output(kind: OutputKind, text: impl Into<String>, data: Value) -> RunOutput {
    let data = match data {
        Value::Null => None,
        Value::Object(m) if m.is_empty() => None,
        other => Some(bounded(&other, 0)),
    };
    RunOutput { kind, text: text.into(), data }
}

/// An object of the given fields, skipping absent and `null` ones.
pub(crate) fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut out = Map::new();
    for key in keys {
        if let Some(x) = v.get(*key).filter(|x| !x.is_null()) {
            out.insert((*key).to_owned(), x.clone());
        }
    }
    Value::Object(out)
}

pub(crate) fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// `v` with long strings cut and large containers shortened, so one event cannot put megabytes
/// into the run log.
fn bounded(v: &Value, depth: usize) -> Value {
    match v {
        Value::String(s) if s.len() > DATA_STRING_MAX => {
            let mut end = DATA_STRING_MAX;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            Value::String(format!("{}…[{} more bytes]", &s[..end], s.len() - end))
        }
        Value::Array(_) | Value::Object(_) if depth >= DATA_DEPTH_MAX => {
            Value::String("…[nested too deep]".to_owned())
        }
        Value::Array(a) => {
            let mut out: Vec<Value> =
                a.iter().take(DATA_ITEMS_MAX).map(|x| bounded(x, depth + 1)).collect();
            if a.len() > DATA_ITEMS_MAX {
                out.push(Value::String(format!("…[{} more items]", a.len() - DATA_ITEMS_MAX)));
            }
            Value::Array(out)
        }
        Value::Object(m) => {
            let mut out: Map<String, Value> = m
                .iter()
                .take(DATA_ITEMS_MAX)
                .map(|(k, x)| (k.clone(), bounded(x, depth + 1)))
                .collect();
            if m.len() > DATA_ITEMS_MAX {
                out.insert(
                    "…".to_owned(),
                    Value::String(format!("[{} more keys]", m.len() - DATA_ITEMS_MAX)),
                );
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// At most `max` characters, with an ellipsis where it was cut.
pub(crate) fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_owned(),
        Some((i, _)) => format!("{}…", &s[..i]),
    }
}

/// One line of at most `max` characters: whitespace runs (newlines included) become one space.
pub(crate) fn one_line(s: &str, max: usize) -> String {
    cut(&s.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

/// A short human summary of a tool call: its name and its most telling argument.
pub(crate) fn tool_summary(name: &str, input: &Value) -> String {
    const TELLING: [&str; 10] = [
        "command",
        "file_path",
        "notebook_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
        "skill",
    ];
    let detail =
        TELLING.iter().find_map(|k| str_at(input, k)).map(str::to_owned).or_else(|| match input {
            Value::Object(m) if m.is_empty() => None,
            Value::Null => None,
            other => Some(other.to_string()),
        });
    match detail {
        Some(d) => one_line(&format!("{name} {d}"), SUMMARY_CHARS),
        None => one_line(name, SUMMARY_CHARS),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn data_is_bounded() {
        let big = "é".repeat(DATA_STRING_MAX);
        let out = output(OutputKind::System, "", json!({ "s": big, "a": vec![1; 150] }));
        let data = out.data.unwrap();
        let s = data["s"].as_str().unwrap();
        assert!(s.len() < DATA_STRING_MAX + 40 && s.ends_with("more bytes]"), "{}", s.len());
        assert_eq!(data["a"].as_array().unwrap().len(), DATA_ITEMS_MAX + 1);
        assert_eq!(output(OutputKind::System, "x", json!({})).data, None);
    }

    #[test]
    fn summaries() {
        let cases = [
            (
                "Bash",
                json!({"command": "ls -la\n  /tmp", "description": "List"}),
                "Bash ls -la /tmp",
            ),
            ("Read", json!({"file_path": "/a/b.txt"}), "Read /a/b.txt"),
            ("mcp__pulse__query", json!({"limit": 3}), "mcp__pulse__query {\"limit\":3}"),
            ("TodoWrite", json!({}), "TodoWrite"),
        ];
        for (name, input, want) in cases {
            assert_eq!(tool_summary(name, &input), want);
        }
        assert_eq!(cut("abcdef", 3), "abc…");
        assert_eq!(cut("abc", 3), "abc");
    }
}
