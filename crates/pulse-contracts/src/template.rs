//! Template syntax for rule prompts and `command` arguments. Rendering lives in `pulse-core`.
//!
//! A template is trusted text with `{{ variable }}` placeholders. Variables:
//! `id`, `type`, `source`, `subject`, `time`, `rule`, `agent`, `prompt` (the rendered prompt;
//! only meaningful in `command` arguments), and `data.<path>` (or bare `<path>`, which means the
//! same) for a field of the event's data, like `{{path}}` or `{{data.repo}}`.
//!
//! There is no escape for a literal `{{`: it always opens a placeholder. Substitution is a single
//! pass, so a value that itself contains `{{…}}` is never expanded.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateVar {
    Id,
    Type,
    Source,
    Subject,
    Time,
    Rule,
    Agent,
    Prompt,
    /// A path into `data`, split on `.`. Numeric segments index arrays.
    Data(Vec<String>),
}

impl fmt::Display for TemplateVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateVar::Id => f.write_str("id"),
            TemplateVar::Type => f.write_str("type"),
            TemplateVar::Source => f.write_str("source"),
            TemplateVar::Subject => f.write_str("subject"),
            TemplateVar::Time => f.write_str("time"),
            TemplateVar::Rule => f.write_str("rule"),
            TemplateVar::Agent => f.write_str("agent"),
            TemplateVar::Prompt => f.write_str("prompt"),
            TemplateVar::Data(p) => write!(f, "data.{}", p.join(".")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplatePart {
    Literal(String),
    Var(TemplateVar),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub parts: Vec<TemplatePart>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("unclosed '{{{{' at byte {0}")]
    Unclosed(usize),
    #[error("empty placeholder '{{{{}}}}' at byte {0}")]
    Empty(usize),
    #[error(
        "invalid variable {0:?}: use id, type, source, subject, time, rule, agent, prompt, or a data path like path or data.repo"
    )]
    BadVariable(String),
}

impl Template {
    pub fn parse(s: &str) -> Result<Template, TemplateError> {
        let mut parts = Vec::new();
        let mut rest = s;
        let mut offset = 0;
        while let Some(start) = rest.find("{{") {
            if start > 0 {
                parts.push(TemplatePart::Literal(rest[..start].to_owned()));
            }
            let after = &rest[start + 2..];
            let end = after.find("}}").ok_or(TemplateError::Unclosed(offset + start))?;
            let name = after[..end].trim();
            if name.is_empty() {
                return Err(TemplateError::Empty(offset + start));
            }
            parts.push(TemplatePart::Var(parse_var(name)?));
            let consumed = start + 2 + end + 2;
            offset += consumed;
            rest = &rest[consumed..];
        }
        if !rest.is_empty() {
            parts.push(TemplatePart::Literal(rest.to_owned()));
        }
        Ok(Template { parts })
    }

    pub fn vars(&self) -> impl Iterator<Item = &TemplateVar> {
        self.parts.iter().filter_map(|p| match p {
            TemplatePart::Var(v) => Some(v),
            TemplatePart::Literal(_) => None,
        })
    }

    pub fn has_vars(&self) -> bool {
        self.vars().next().is_some()
    }
}

fn parse_var(name: &str) -> Result<TemplateVar, TemplateError> {
    Ok(match name {
        "id" => TemplateVar::Id,
        "type" => TemplateVar::Type,
        "source" => TemplateVar::Source,
        "subject" => TemplateVar::Subject,
        "time" => TemplateVar::Time,
        "rule" => TemplateVar::Rule,
        "agent" => TemplateVar::Agent,
        "prompt" => TemplateVar::Prompt,
        other => TemplateVar::Data(
            parse_data_path(other.strip_prefix("data.").unwrap_or(other))
                .ok_or_else(|| TemplateError::BadVariable(name.to_owned()))?,
        ),
    })
}

/// Splits `a.b.0` into segments of `[A-Za-z0-9_-]`. Shared with the `where` key syntax.
pub fn parse_data_path(s: &str) -> Option<Vec<String>> {
    let segs: Vec<String> = s.split('.').map(str::to_owned).collect();
    let ok = !s.is_empty()
        && segs.iter().all(|seg| {
            !seg.is_empty()
                && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
    ok.then_some(segs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_templates() {
        let t = Template::parse("Summarize {{subject}} at {{ data.path }} ({{size}}).").unwrap();
        assert_eq!(
            t.parts,
            vec![
                TemplatePart::Literal("Summarize ".into()),
                TemplatePart::Var(TemplateVar::Subject),
                TemplatePart::Literal(" at ".into()),
                TemplatePart::Var(TemplateVar::Data(vec!["path".into()])),
                TemplatePart::Literal(" (".into()),
                TemplatePart::Var(TemplateVar::Data(vec!["size".into()])),
                TemplatePart::Literal(").".into()),
            ]
        );
        assert!(!Template::parse("no vars } here").unwrap().has_vars());
        let errors = [
            ("open {{subject", TemplateError::Unclosed(5)),
            ("x {{ }} y", TemplateError::Empty(2)),
            ("{{a b}}", TemplateError::BadVariable("a b".into())),
            ("{{data.}}", TemplateError::BadVariable("data.".into())),
            ("{{$(rm)}}", TemplateError::BadVariable("$(rm)".into())),
        ];
        for (src, want) in errors {
            assert_eq!(Template::parse(src).unwrap_err(), want, "{src}");
        }
    }
}
