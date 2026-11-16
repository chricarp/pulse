//! Parsing, validation and resolution of config files.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    AdapterSettings, AgentConfig, AgentKind, CalendarSourceSettings, Config, ConfigFile,
    ConfigOrigin, Limits, RuleConfig, ScheduleSourceSettings, SourceConfig, SourceSettings,
    Sourced, WhereCondition, WhereField, WhereScalar,
};
use crate::pattern::{expand_path, looks_like_path, parse_path_glob, parse_text_glob};
use crate::template::{Template, TemplateVar};

/// One problem in a config file, located as precisely as we can.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct ConfigError {
    pub file: Option<String>,
    /// 1-based.
    pub line: Option<u32>,
    /// 1-based, in characters.
    pub column: Option<u32>,
    /// `rule "new-lecture"`, `agent "study"`, `[limits]`.
    pub item: Option<String>,
    /// `debounce`, `run.agent`, `allow[1]`.
    pub field: Option<String>,
    /// What is wrong and what was expected.
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{file}")?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
                if let Some(col) = self.column {
                    write!(f, ":{col}")?;
                }
            }
            f.write_str(": ")?;
        }
        match (&self.item, &self.field) {
            (Some(item), Some(field)) => write!(f, "{item}, field `{field}`: ")?,
            (Some(item), None) => write!(f, "{item}: ")?,
            (None, Some(field)) => write!(f, "field `{field}`: ")?,
            (None, None) => {}
        }
        f.write_str(&self.message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct ConfigErrors(pub Vec<ConfigError>);

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

/// Parses and validates one config file. `home` expands `~`; it is never read from the
/// environment here.
pub fn parse_config_file(
    text: &str,
    origin: &ConfigOrigin,
    home: &Path,
) -> Result<ConfigFile, ConfigErrors> {
    let index = TomlIndex::new(text);
    let file_label = origin.path().display().to_string();
    let file: ConfigFile = toml::from_str(text).map_err(|e| {
        let mut err = ConfigError {
            file: Some(file_label.clone()),
            line: None,
            column: None,
            item: None,
            field: None,
            message: e.message().trim().to_owned(),
        };
        if let Some(span) = e.span() {
            let (line, col) = index.line_col(span.start);
            err.line = Some(line);
            err.column = Some(col);
            err.item = index.item_at_line(line);
            err.field = index.key_at_line(line);
        }
        ConfigErrors(vec![err])
    })?;
    let mut errors = validate_file(&file, origin, home);
    for e in &mut errors {
        e.file = Some(file_label.clone());
        if e.line.is_none() {
            e.line = index.locate(e);
        }
    }
    if errors.is_empty() { Ok(file) } else { Err(ConfigErrors(errors)) }
}

/// Validates a file's definitions on their own (cross-file references are checked by
/// [`resolve`]). Errors carry item and field but no file or line.
pub fn validate_file(file: &ConfigFile, origin: &ConfigOrigin, home: &Path) -> Vec<ConfigError> {
    let mut v = Validator { errors: Vec::new(), home, base: origin.base_dir() };
    if origin.project().is_some() {
        for (present, section) in [
            (file.daemon.is_some(), "[daemon]"),
            (file.limits.is_some(), "[limits]"),
            (file.retention.is_some(), "[retention]"),
        ] {
            if present {
                v.push(
                    Some(section.to_owned()),
                    None,
                    "is only allowed in the user's pulse.toml, not in a project file",
                );
            }
        }
    }
    if let Some(limits) = &file.limits {
        v.limits(limits);
    }
    v.unique("source", file.sources.iter().map(|s| s.name.as_str()));
    v.unique("agent", file.agents.iter().map(|a| a.name.as_str()));
    v.unique("rule", file.rules.iter().map(|r| r.name.as_str()));
    for s in &file.sources {
        v.source(s);
    }
    for a in &file.agents {
        v.agent(a);
    }
    for r in &file.rules {
        v.rule(r);
    }
    v.errors
}

/// Checks a definition name: 1–64 of `[a-z0-9_-]`, starting with a letter or digit.
pub fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid name {name:?}: use 1-64 lowercase letters, digits, '-' or '_', starting with a letter or digit"
        ))
    }
}

/// Combines the user file with trusted project files. Project definitions are renamed
/// `<project>/<name>`; a project rule's `run.agent` refers to the project's own agent of that
/// name if there is one, otherwise to the user's.
pub fn resolve(
    user: Sourced<ConfigFile>,
    projects: Vec<Sourced<ConfigFile>>,
) -> Result<Config, ConfigErrors> {
    let mut errors = Vec::new();
    let mut config = Config {
        daemon: user.config.daemon.clone().unwrap_or_default(),
        limits: user.config.limits.clone().unwrap_or_default(),
        retention: user.config.retention.clone().unwrap_or_default(),
        sources: Vec::new(),
        agents: Vec::new(),
        rules: Vec::new(),
    };
    let mut seen_projects = BTreeSet::new();
    for p in &projects {
        if let Some(name) = p.origin.project() {
            if let Err(msg) = validate_name(name) {
                errors.push(err_at(&p.origin, None, None, format!("project {msg}")));
            }
            if !seen_projects.insert(name.to_owned()) {
                errors.push(err_at(
                    &p.origin,
                    None,
                    None,
                    format!("two trusted projects are both named {name:?}"),
                ));
            }
        }
    }
    let user_agents: HashMap<&str, AgentKind> =
        user.config.agents.iter().map(|a| (a.name.as_str(), a.kind())).collect();
    let files = std::iter::once(&user).chain(projects.iter());
    for file in files {
        let prefix = |name: &str| match file.origin.project() {
            Some(p) => format!("{p}/{name}"),
            None => name.to_owned(),
        };
        let local_agents: HashMap<&str, AgentKind> =
            file.config.agents.iter().map(|a| (a.name.as_str(), a.kind())).collect();
        for s in &file.config.sources {
            let mut s = s.clone();
            s.name = prefix(&s.name);
            config.sources.push(Sourced { origin: file.origin.clone(), config: s });
        }
        for a in &file.config.agents {
            let mut a = a.clone();
            a.name = prefix(&a.name);
            config.agents.push(Sourced { origin: file.origin.clone(), config: a });
        }
        for r in &file.config.rules {
            let mut r = r.clone();
            let item = Some(format!("rule {:?}", r.name));
            let target = r.run.agent.clone();
            let resolved =
                if file.origin.project().is_some() && local_agents.contains_key(target.as_str()) {
                    Some((prefix(&target), local_agents[target.as_str()]))
                } else {
                    user_agents.get(target.as_str()).map(|k| (target.clone(), *k))
                };
            match resolved {
                None => errors.push(err_at(
                    &file.origin,
                    item,
                    Some("run.agent"),
                    format!("no agent named {target:?} is defined"),
                )),
                Some((_, kind)) if !kind.is_runnable() => errors.push(err_at(
                    &file.origin,
                    item,
                    Some("run.agent"),
                    format!("agent {target:?} uses the mcp adapter, which Pulse does not run; rules cannot target it"),
                )),
                Some((_, kind)) if kind != AgentKind::Command && r.run.prompt.is_none() => {
                    errors.push(err_at(
                        &file.origin,
                        item,
                        Some("run.prompt"),
                        "is required for claude-code and codex agents".to_owned(),
                    ));
                }
                Some((full, _)) => {
                    r.name = prefix(&r.name);
                    r.run.agent = full;
                    config.rules.push(Sourced { origin: file.origin.clone(), config: r });
                }
            }
        }
    }
    if errors.is_empty() { Ok(config) } else { Err(ConfigErrors(errors)) }
}

fn err_at(
    origin: &ConfigOrigin,
    item: Option<String>,
    field: Option<&str>,
    message: String,
) -> ConfigError {
    ConfigError {
        file: Some(origin.path().display().to_string()),
        line: None,
        column: None,
        item,
        field: field.map(str::to_owned),
        message,
    }
}

struct Validator<'a> {
    errors: Vec<ConfigError>,
    home: &'a Path,
    base: Option<&'a Path>,
}

impl Validator<'_> {
    fn push(&mut self, item: Option<String>, field: Option<&str>, message: impl Into<String>) {
        self.errors.push(ConfigError {
            file: None,
            line: None,
            column: None,
            item,
            field: field.map(str::to_owned),
            message: message.into(),
        });
    }

    fn unique<'n>(&mut self, kind: &str, names: impl Iterator<Item = &'n str>) {
        let mut seen = BTreeSet::new();
        for name in names {
            if let Err(msg) = validate_name(name) {
                self.push(Some(format!("{kind} {name:?}")), Some("name"), msg);
            }
            if !seen.insert(name) {
                self.push(
                    Some(format!("{kind} {name:?}")),
                    Some("name"),
                    format!("another {kind} has the same name"),
                );
            }
        }
    }

    fn path(&mut self, item: &Option<String>, field: &str, raw: &str) {
        if let Err(e) = expand_path(raw, self.home, self.base) {
            self.push(item.clone(), Some(field), e.to_string());
        }
    }

    fn positive(&mut self, item: &Option<String>, field: &str, d: Option<crate::time::Duration>) {
        if d.is_some_and(|d| d.is_zero()) {
            self.push(item.clone(), Some(field), "must be greater than zero");
        }
    }

    fn range(&mut self, item: &Option<String>, field: &str, value: u64, min: u64, max: u64) {
        if value < min || value > max {
            self.push(
                item.clone(),
                Some(field),
                format!("expected a value from {min} to {max}, got {value}"),
            );
        }
    }

    fn limits(&mut self, l: &Limits) {
        let item = Some("[limits]".to_owned());
        self.range(&item, "max_chain_depth", l.max_chain_depth.into(), 1, 32);
        self.range(&item, "dedupe_capacity", l.dedupe_capacity.into(), 1, 1_000_000);
        self.range(&item, "debounce_capacity", l.debounce_capacity.into(), 1, 1_000_000);
        self.positive(&item, "dedupe_ttl", Some(l.dedupe_ttl));
        self.range(
            &item,
            "circuit_breaker.max_fires",
            l.circuit_breaker.max_fires.into(),
            1,
            10_000,
        );
        self.positive(&item, "circuit_breaker.window", Some(l.circuit_breaker.window));
        self.positive(&item, "circuit_breaker.open_for", Some(l.circuit_breaker.open_for));
        self.range(&item, "prompt.inline_max_chars", l.prompt.inline_max_chars.into(), 16, 100_000);
        self.range(&item, "prompt.field_max_bytes", l.prompt.field_max_bytes.into(), 64, 1_000_000);
        self.range(&item, "prompt.data_max_bytes", l.prompt.data_max_bytes.into(), 1024, 4_000_000);
        self.range(&item, "run_output_max_bytes", l.run_output_max_bytes, 4096, 1 << 32);
    }

    fn source(&mut self, s: &SourceConfig) {
        let item = Some(format!("source {:?}", s.name));
        match &s.settings {
            SourceSettings::Fs(fs) => {
                self.path(&item, "path", &fs.path);
                for (i, pat) in fs.ignore.iter().enumerate() {
                    if let Err(e) = parse_text_glob(pat) {
                        self.push(item.clone(), Some(&format!("ignore[{i}]")), e.to_string());
                    }
                }
                self.positive(&item, "quiet", fs.quiet);
                if let Some(t) = fs.burst_threshold {
                    self.range(&item, "burst_threshold", t.into(), 1, 1_000_000);
                }
            }
            SourceSettings::Git(g) => self.path(&item, "path", &g.path),
            SourceSettings::Schedule(ScheduleSourceSettings { cron, every, timezone: _ }) => {
                match (cron, every) {
                    (Some(_), Some(_)) | (None, None) => {
                        self.push(item.clone(), None, "set exactly one of `cron` and `every`");
                    }
                    (Some(c), None) => {
                        if !looks_like_cron(c) {
                            self.push(
                                item.clone(),
                                Some("cron"),
                                format!("expected five fields like \"0 9 * * 1-5\", got {c:?}"),
                            );
                        }
                    }
                    (None, Some(_)) => self.positive(&item, "every", *every),
                }
            }
            SourceSettings::Calendar(CalendarSourceSettings { url, path, refresh, lead }) => {
                match (url, path) {
                    (Some(_), Some(_)) | (None, None) => {
                        self.push(item.clone(), None, "set exactly one of `url` and `path`");
                    }
                    (Some(u), None) => {
                        if !(u.starts_with("https://") || u.starts_with("http://")) {
                            self.push(
                                item.clone(),
                                Some("url"),
                                format!("expected an http(s) URL, got {u:?}"),
                            );
                        }
                    }
                    (None, Some(p)) => self.path(&item, "path", p),
                }
                self.positive(&item, "refresh", *refresh);
                let _ = lead;
            }
        }
    }

    fn agent(&mut self, a: &AgentConfig) {
        let item = Some(format!("agent {:?}", a.name));
        for (i, scope) in a.allow.iter().enumerate() {
            if let Some(p) = &scope.path {
                if let Err(e) = parse_path_glob(p, self.home) {
                    self.push(item.clone(), Some(&format!("allow[{i}]")), e.to_string());
                }
            }
        }
        if let Some(cwd) = &a.cwd {
            self.path(&item, "cwd", cwd);
        }
        self.positive(&item, "timeout", Some(a.timeout));
        self.range(&item, "max_concurrent", a.max_concurrent.into(), 1, 64);
        self.range(&item, "max_queued", a.max_queued.into(), 0, 10_000);
        for key in a.env.keys() {
            let valid = key.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if !valid {
                self.push(
                    item.clone(),
                    Some("env"),
                    format!("invalid environment variable name {key:?}"),
                );
            } else if key.starts_with("PULSE_") {
                self.push(
                    item.clone(),
                    Some("env"),
                    format!("{key:?}: names starting with PULSE_ are reserved"),
                );
            }
        }
        if let AdapterSettings::Command(c) = &a.adapter {
            match c.command.first() {
                None => self.push(item.clone(), Some("command"), "expected at least the program, like [\"make\", \"study\"]"),
                Some(program) if program.contains("{{") => self.push(
                    item.clone(),
                    Some("command[0]"),
                    "the program cannot contain placeholders; event content must never choose what runs",
                ),
                Some(_) => {}
            }
            for (i, arg) in c.command.iter().enumerate().skip(1) {
                if let Err(e) = Template::parse(arg) {
                    self.push(item.clone(), Some(&format!("command[{i}]")), e.to_string());
                }
            }
        }
    }

    fn rule(&mut self, r: &RuleConfig) {
        let item = Some(format!("rule {:?}", r.name));
        for (key, cond) in &r.where_ {
            let field = format!("where.{key}");
            if WhereField::parse(key).is_none() {
                self.push(
                    item.clone(),
                    Some(&field),
                    "invalid key: use subject, source, type, or a data path like path or data.repo",
                );
                continue;
            }
            let mut check = |s: &str| {
                let res = if looks_like_path(s) {
                    parse_path_glob(s, self.home).map(drop)
                } else {
                    parse_text_glob(s).map(drop)
                };
                if let Err(e) = res {
                    self.errors.push(ConfigError {
                        file: None,
                        line: None,
                        column: None,
                        item: item.clone(),
                        field: Some(field.clone()),
                        message: e.to_string(),
                    });
                }
            };
            match cond {
                WhereCondition::Match(WhereScalar::String(s)) => check(s),
                WhereCondition::AnyOf(items) => {
                    for i in items {
                        if let WhereScalar::String(s) = i {
                            check(s);
                        }
                    }
                }
                WhereCondition::Ops(ops) => {
                    if let Some(g) = &ops.glob {
                        check(g);
                    }
                }
                WhereCondition::Match(_) => {}
            }
        }
        self.positive(&item, "debounce", r.debounce);
        self.positive(&item, "cooldown", r.cooldown);
        if let Err(msg) = validate_name(&r.run.agent) {
            self.push(item.clone(), Some("run.agent"), msg);
        }
        if let Some(prompt) = &r.run.prompt {
            match Template::parse(prompt) {
                Err(e) => self.push(item.clone(), Some("run.prompt"), e.to_string()),
                Ok(t) if t.vars().any(|v| *v == TemplateVar::Prompt) => {
                    self.push(
                        item.clone(),
                        Some("run.prompt"),
                        "{{prompt}} is only available in command arguments",
                    );
                }
                Ok(_) => {}
            }
        }
    }
}

fn looks_like_cron(c: &str) -> bool {
    let fields: Vec<&str> = c.split_whitespace().collect();
    fields.len() == 5
        && fields.iter().all(|f| {
            f.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'*' | b'/' | b',' | b'-'))
        })
}

/// Line-level knowledge of a TOML text, for pointing errors at the right place without a span
/// for every value.
struct TomlIndex<'a> {
    text: &'a str,
    lines: Vec<&'a str>,
    /// (header, array index, first line, last line), lines 1-based.
    blocks: Vec<(String, usize, u32, u32)>,
}

impl<'a> TomlIndex<'a> {
    fn new(text: &'a str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let mut blocks: Vec<(String, usize, u32, u32)> = Vec::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            let header = t
                .strip_prefix("[[")
                .and_then(|h| h.split("]]").next())
                .map(|h| (h.trim().to_owned(), true))
                .or_else(|| {
                    t.strip_prefix('[')
                        .and_then(|h| h.split(']').next())
                        .map(|h| (h.trim().to_owned(), false))
                });
            if let Some((name, is_array)) = header {
                let line_no = u32::try_from(i + 1).unwrap_or(u32::MAX);
                if let Some(last) = blocks.last_mut() {
                    last.3 = line_no.saturating_sub(1);
                }
                let idx = if is_array {
                    let c = counts.entry(name.clone()).or_default();
                    *c += 1;
                    *c - 1
                } else {
                    0
                };
                blocks.push((name, idx, line_no, u32::MAX));
            }
        }
        TomlIndex { text, lines, blocks }
    }

    fn line_col(&self, offset: usize) -> (u32, u32) {
        let before = &self.text[..offset.min(self.text.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        (u32::try_from(line).unwrap_or(u32::MAX), u32::try_from(col).unwrap_or(u32::MAX))
    }

    fn block_at(&self, line: u32) -> Option<&(String, usize, u32, u32)> {
        self.blocks.iter().find(|b| b.2 <= line && line <= b.3)
    }

    fn block_name(&self, b: &(String, usize, u32, u32)) -> Option<String> {
        let (start, end) = (b.2 as usize, (b.3 as usize).min(self.lines.len()));
        self.lines.get(start..end)?.iter().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "name").then(|| v.trim().trim_matches('"').to_owned())
        })
    }

    fn item_at_line(&self, line: u32) -> Option<String> {
        let b = self.block_at(line)?;
        let kind = b.0.as_str();
        if ["rule", "agent", "source"].contains(&kind) {
            Some(match self.block_name(b) {
                Some(name) => format!("{kind} {name:?}"),
                None => format!("{kind} #{}", b.1 + 1),
            })
        } else {
            Some(format!("[{kind}]"))
        }
    }

    fn key_at_line(&self, line: u32) -> Option<String> {
        let l = self.lines.get(usize::try_from(line).ok()?.checked_sub(1)?)?;
        let (k, _) = l.split_once('=')?;
        let k = k.trim().trim_matches('"');
        (!k.is_empty() && !k.starts_with('[')).then(|| k.to_owned())
    }

    /// Finds the line of a validation error from its item and field.
    fn locate(&self, e: &ConfigError) -> Option<u32> {
        let item = e.item.as_deref()?;
        let block = if let Some(section) = item.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
        {
            self.blocks.iter().find(|b| b.0 == section)?
        } else {
            let (kind, name) = item.split_once(' ')?;
            let name = name.trim_matches('"');
            self.blocks
                .iter()
                .find(|b| b.0 == kind && self.block_name(b).as_deref() == Some(name))?
        };
        let Some(field) = &e.field else { return Some(block.2) };
        let key = field.split(['.', '[']).next().unwrap_or(field);
        let (start, end) = (block.2 as usize, (block.3 as usize).min(self.lines.len()));
        let found = self.lines.get(start..end)?.iter().position(|l| {
            l.split_once('=').is_some_and(|(k, _)| k.trim().trim_matches('"') == key)
        });
        Some(found.map_or(block.2, |i| block.2 + 1 + u32::try_from(i).unwrap_or(0)))
    }
}

#[cfg(test)]
mod tests;
