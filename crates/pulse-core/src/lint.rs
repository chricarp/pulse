//! Config warnings: definitions that are valid but cannot do what they look like they do. A rule
//! that can never fire is otherwise silent until someone wonders why nothing happens (or, when
//! it does match, shows up as a `permission_denied` on every delivery). The daemon reports these
//! next to config errors, at the moment the file is saved.
//!
//! Only what can be decided from the config and the catalog is here; checks that need the
//! filesystem (is the agent's program installed, does its working directory exist) are the
//! daemon's.

use std::path::Path;

use pulse_contracts::catalog::{CATALOG, EXTERNAL_PATTERN, entry};
use pulse_contracts::config::{
    AgentConfig, Config, ConfigError, ConfigOrigin, RuleConfig, Scope, WhereCondition, WhereScalar,
};
use pulse_contracts::pattern::{expand_path, looks_like_path, parse_type_glob};

use crate::glob;

/// Attributes every event has, whatever its type.
const ENVELOPE_KEYS: &[&str] = &["subject", "source", "type", "id", "time"];

/// Warnings for rules that can never fire. Archived rules and rules whose agent is missing (an
/// error, reported elsewhere) are skipped.
pub fn lint(config: &Config, home: &Path) -> Vec<ConfigError> {
    let mut out = Vec::new();
    for rule in config.rules.iter().filter(|r| !r.config.archived) {
        let Some(agent) = config.agent(&rule.config.run.agent) else { continue };
        lint_rule(&rule.config, &agent.config, &rule.origin, home, &mut out);
    }
    out
}

fn warning(origin: &ConfigOrigin, rule: &str, field: &str, message: String) -> ConfigError {
    ConfigError {
        file: Some(origin.path().display().to_string()),
        line: None,
        column: None,
        item: Some(format!("rule {rule:?}")),
        field: Some(field.to_owned()),
        message,
    }
}

fn type_matches(pattern: &str, ty: &str) -> bool {
    parse_type_glob(pattern).is_ok_and(|g| glob::matches(&g, ty))
}

fn lint_rule(
    rule: &RuleConfig,
    agent: &AgentConfig,
    origin: &ConfigOrigin,
    home: &Path,
    out: &mut Vec<ConfigError>,
) {
    let on: Vec<&str> = rule.on.0.iter().map(|t| t.as_str()).collect();
    let builtin: Vec<&str> = CATALOG
        .iter()
        .map(|e| e.type_)
        .filter(|t| *t != EXTERNAL_PATTERN && on.iter().any(|p| type_matches(p, t)))
        .collect();
    let external: Vec<&str> = on.iter().copied().filter(|p| p.starts_with("ext.")).collect();
    if builtin.is_empty() && external.is_empty() {
        out.push(warning(
            origin,
            &rule.name,
            "on",
            format!(
                "no event type matches {}, so this rule never fires (external types start with `ext.`; `pulse events` and the catalog list the others)",
                on.join(", ")
            ),
        ));
        return;
    }

    // `where` keys that no event this rule listens to has: a missing field never matches.
    if external.is_empty() {
        for key in rule.where_.keys() {
            let field = key.strip_prefix("data.").unwrap_or(key);
            let first = field.split('.').next().unwrap_or(field);
            if !key.starts_with("data.") && ENVELOPE_KEYS.contains(&first) {
                continue;
            }
            let known = builtin
                .iter()
                .any(|t| entry(t).is_some_and(|e| (e.example)().get(first).is_some()));
            if !known {
                out.push(warning(
                    origin,
                    &rule.name,
                    &format!("where.{key}"),
                    format!(
                        "no {} event has a field `{first}`, so this rule never matches",
                        builtin.join(" / ")
                    ),
                ));
            }
        }
    }

    // The agent's scopes must be able to cover at least one of the rule's events.
    let hint = path_hint(rule, home);
    let coverable = builtin.iter().any(|t| {
        let has_paths = entry(t).is_some_and(|e| !e.paths.is_empty());
        covers(&agent.allow, t, has_paths, hint.as_deref(), home)
    }) || external.iter().any(|p| {
        // A literal external type can be checked; a glob over the namespace is given the
        // benefit of the doubt.
        p.contains(['*', '?', '{']) || covers(&agent.allow, p, false, None, home)
    });
    if !coverable {
        let allow: Vec<String> = agent.allow.iter().map(ToString::to_string).collect();
        let types = if builtin.is_empty() { external.join(", ") } else { builtin.join(", ") };
        let under = hint.map(|h| format!(" under {h}")).unwrap_or_default();
        out.push(warning(
            origin,
            &rule.name,
            "run.agent",
            format!(
                "this rule can never fire: agent {:?} is allowed [{}], which does not cover {types}{under}. Every matching event would be blocked as permission_denied; widen the agent's `allow`",
                agent.name,
                allow.join(", ")
            ),
        ));
    }
}

/// The literal directory prefix of the rule's `where` path (`path` or `subject`), expanded:
/// `~/University/**/*.pdf` gives `/home/me/University`.
fn path_hint(rule: &RuleConfig, home: &Path) -> Option<String> {
    ["path", "data.path", "subject"].iter().find_map(|k| match rule.where_.get(*k)? {
        WhereCondition::Match(WhereScalar::String(s)) if looks_like_path(s) => literal_dir(s, home),
        _ => None,
    })
}

/// The part of a path glob before its first wildcard, cut back to a whole directory.
fn literal_dir(glob: &str, home: &Path) -> Option<String> {
    let cut = glob.find(['*', '?', '{', '[']).unwrap_or(glob.len());
    let lit = &glob[..cut];
    let dir = if cut == glob.len() { lit } else { lit.rsplit_once('/').map_or("", |(d, _)| d) };
    let dir = if dir.is_empty() { "/" } else { dir };
    expand_path(dir, home, None).ok()
}

/// Two directories overlap when one contains the other.
fn overlap(a: &str, b: &str) -> bool {
    let within = |x: &str, y: &str| {
        x == y || y == "/" || x.strip_prefix(y).is_some_and(|rest| rest.starts_with('/'))
    };
    within(a, b) || within(b, a)
}

fn covers(allow: &[Scope], ty: &str, has_paths: bool, hint: Option<&str>, home: &Path) -> bool {
    allow.iter().any(|s| {
        if !type_matches(s.types.as_str(), ty) {
            return false;
        }
        match &s.path {
            None => true,
            // A path-qualified grant never covers an event without paths.
            Some(_) if !has_paths => false,
            Some(p) => match (hint, literal_dir(p, home)) {
                (Some(h), Some(g)) => overlap(h, &g),
                _ => true,
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use pulse_contracts::config::{ConfigOrigin, Sourced, parse_config_file, resolve};

    use super::*;

    fn warnings(text: &str) -> Vec<String> {
        let home = Path::new("/home/me");
        let origin = ConfigOrigin::User { path: "/p/pulse.toml".into() };
        let file = parse_config_file(text, &origin, home).unwrap();
        let config = resolve(Sourced { origin, config: file }, vec![]).unwrap();
        lint(&config, home)
            .into_iter()
            .map(|e| format!("{}: {}", e.field.unwrap(), e.message))
            .collect()
    }

    fn rule(on: &str, where_: &str, allow: &str) -> String {
        format!(
            "[[agent]]\nname = \"a\"\nadapter = \"command\"\ncommand = [\"true\"]\nallow = [{allow}]\n\n\
             [[rule]]\nname = \"r\"\non = {on:?}\nwhere = {{ {where_} }}\nrun = {{ agent = \"a\" }}\n"
        )
    }

    #[test]
    fn rules_that_can_never_fire() {
        // (on, where, allow, expected warning fields)
        let cases: &[(&str, &str, &str, &[&str])] = &[
            // The lecture-notes shape: fine.
            (
                "fs.file.created",
                "path = \"~/University/**/*.pdf\"",
                "\"fs.file.created:~/University/**\"",
                &[],
            ),
            // A typo in the type.
            ("fs.file.create", "", "\"fs.*\"", &["on"]),
            // A typo in a field.
            ("fs.file.created", "pth = \"~/x/**\"", "\"fs.*\"", &["where.pth"]),
            (
                "process.exited",
                "success = false, cwd = \"~/code/**\"",
                "\"process.exited:~/code/**\"",
                &[],
            ),
            ("process.exited", "\"data.exit_code\" = 1", "\"process.*\"", &[]),
            // The scope is for another type.
            ("fs.file.created", "", "\"git.*\"", &["run.agent"]),
            // The scope is for another folder.
            (
                "fs.file.created",
                "path = \"~/University/**\"",
                "\"fs.*:~/Downloads/**\"",
                &["run.agent"],
            ),
            // A scope above or below the rule's folder can still cover some events.
            ("fs.file.created", "path = \"~/University/A/**\"", "\"fs.*:~/University/**\"", &[]),
            ("fs.file.created", "path = \"~/University/**\"", "\"fs.*:~/University/A/**\"", &[]),
            // Schedule events carry no paths: a path-qualified grant never covers them.
            ("schedule.fired", "", "\"schedule.*:~/x/**\"", &["run.agent"]),
            ("schedule.fired", "", "\"schedule.*\"", &[]),
            // External types: literal ones are checked, globs are not.
            ("ext.ci.failed", "", "\"ext.ci.*\"", &[]),
            ("ext.ci.failed", "", "\"ext.deploy.*\"", &["run.agent"]),
            ("ext.ci.*", "", "\"fs.*\"", &[]),
            // A glob over several types is fine if any of them is covered.
            ("fs.*", "", "\"fs.file.deleted\"", &[]),
        ];
        for (on, where_, allow, want) in cases {
            let got = warnings(&rule(on, where_, allow));
            let fields: Vec<&str> = got.iter().map(|w| w.split(':').next().unwrap()).collect();
            assert_eq!(&fields, want, "{on} {where_} {allow}: {got:#?}");
        }
    }
}
