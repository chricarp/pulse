//! Config: loading `$PULSE_HOME/pulse.toml` and trusted project files, keeping the last good
//! version, project trust, and format-preserving write-back.
//!
//! - **Last good.** If the user file stops parsing, its last good version stays in effect and
//!   the errors are reported. If the files parse but do not resolve together (a rule names a
//!   missing agent), the whole previous configuration stays in effect.
//! - **Trust.** A project file is loaded only while its SHA-256 equals the hash recorded when it
//!   was trusted. There is no "last good" for a project: changed content is inert until trusted
//!   again. Write-back through the daemon (the user acting with the daemon token) re-binds the
//!   trust to the new content.
//! - **Write-back** edits the owning file with [`edit`], validates the result as the loader
//!   would, and replaces the file atomically. The file stays the only copy of a definition.

mod diff;
pub(crate) mod edit;
pub(crate) mod watch;

use std::path::{Path, PathBuf};

use pulse_contracts::Timestamp;
use pulse_contracts::config::{
    AdapterSettings, Config, ConfigError, ConfigErrors, ConfigFile, ConfigOrigin, RuleConfig,
    Sourced, parse_config_file, resolve, validate_file, validate_name,
};
use pulse_contracts::pattern::expand_path;
use pulse_contracts::rpc::ProjectReview;
use pulse_contracts::status::{Project, TrustState};
use pulse_core::Routing;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ServiceError, ServiceResult};
use crate::home::PulseHome;

pub(crate) use edit::DefKind;

/// The configuration in effect.
#[derive(Debug)]
pub struct Snapshot {
    pub config: Config,
    pub routing: Routing,
    /// The parsed files it was resolved from (user file first), as written.
    pub(crate) files: Vec<Sourced<ConfigFile>>,
    /// Definitions that could not work as written when this was loaded (see [`warnings`]).
    pub warnings: Vec<ConfigError>,
}

impl Snapshot {
    /// The configuration of an empty user file.
    pub(crate) fn empty(home: &PulseHome, user_home: &Path) -> Result<Snapshot, ConfigErrors> {
        build(vec![Sourced { origin: user_origin(home), config: ConfigFile::default() }], user_home)
    }

    /// The config and routing that would result from adding or replacing `rule` (a bare name)
    /// in the user file or in project `project`. For testing unsaved rules.
    pub(crate) fn with_draft(
        &self,
        rule: &RuleConfig,
        project: Option<&str>,
        user_home: &Path,
    ) -> ServiceResult<(Config, Routing, String)> {
        let mut files = self.files.clone();
        let file = files
            .iter_mut()
            .find(|f| f.origin.project() == project)
            .ok_or_else(|| ServiceError::not_found("trusted project", project.unwrap_or("")))?;
        file.config.rules.retain(|r| r.name != rule.name);
        file.config.rules.push(rule.clone());
        let errors = validate_file(&file.config, &file.origin, user_home);
        if !errors.is_empty() {
            return Err(ServiceError::ConfigInvalid(ConfigErrors(errors)));
        }
        let full = match project {
            Some(p) => format!("{p}/{}", rule.name),
            None => rule.name.clone(),
        };
        let mut files = files.into_iter();
        let user = files.next().ok_or_else(|| ServiceError::Internal("no user file".into()))?;
        let config = resolve(user, files.collect()).map_err(ServiceError::ConfigInvalid)?;
        let routing = Routing::compile(&config, user_home).map_err(ServiceError::ConfigInvalid)?;
        Ok((config, routing, full))
    }
}

pub(crate) fn user_origin(home: &PulseHome) -> ConfigOrigin {
    ConfigOrigin::User { path: home.config_file() }
}

/// A trusted project, as persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TrustRecord {
    pub name: String,
    pub dir: PathBuf,
    pub hash: String,
    pub trusted_at: Timestamp,
    /// The trusted text, kept (up to [`KEEP_TRUSTED_TEXT`]) so a review of a changed file can
    /// say what differs. Absent in records written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Trusted texts up to this size are kept for showing what changed.
const KEEP_TRUSTED_TEXT: usize = 64 * 1024;
/// Project files larger than this are refused: nobody reviews them, and a review must fit in
/// one RPC response.
pub(crate) const MAX_PROJECT_FILE: usize = 1024 * 1024;
/// The user's own `pulse.toml` gets more room; the bound only keeps a mistake (a link to a
/// device, a log file) from being read whole.
const MAX_USER_FILE: usize = 8 * 1024 * 1024;

fn kept_text(text: &str) -> Option<String> {
    (text.len() <= KEEP_TRUSTED_TEXT).then(|| text.to_owned())
}

impl TrustRecord {
    pub(crate) fn file(&self) -> PathBuf {
        self.dir.join("pulse.toml")
    }
    pub(crate) fn origin(&self) -> ConfigOrigin {
        ConfigOrigin::Project { name: self.name.clone(), dir: self.dir.clone(), path: self.file() }
    }
}

/// The `kv` key holding the trust records.
pub(crate) const TRUST_KEY: &str = "trusted_projects";

pub(crate) fn hash(bytes: &[u8]) -> String {
    crate::auth::hex(&Sha256::digest(bytes))
}

/// A definition's revision: a hash of its meaning as written (the typed definition, so a
/// change of formatting or comments is not a new revision, and a write-back that changes no
/// meaning keeps it). Handed out with every rule, agent and source; edits name the revision
/// they were based on.
pub(crate) fn revision<T: Serialize>(def: &T) -> String {
    // Serializing config types cannot fail: they hold strings, numbers, lists and string maps.
    let json = serde_json::to_vec(def).unwrap_or_default();
    hash(&json)[..16].to_owned()
}

/// What one load produced.
#[derive(Debug)]
pub(crate) struct Loaded {
    /// `None` when the files did not resolve together; the previous config stays.
    pub snapshot: Option<Snapshot>,
    pub errors: Vec<ConfigError>,
    pub projects: Vec<Project>,
}

/// Reads and validates config files. Owned by the daemon behind an async mutex, which also
/// serializes write-back.
#[derive(Debug)]
pub(crate) struct Loader {
    home: PulseHome,
    user_home: PathBuf,
    /// The user file's last good parse and the hash of its text.
    user_good: Option<(String, ConfigFile)>,
    pub(crate) trust: Vec<TrustRecord>,
}

/// A config file's text, or `None` if it does not exist. A project's `pulse.toml` is read
/// before anyone has trusted it, so the file is whatever a repository's author made it: a FIFO
/// would block the reload (which holds the loader lock) forever, and a link to `/dev/zero`
/// would be read until memory ran out. So only a regular file is read (links are followed: a
/// linked file may be reviewed and trusted by its content), without blocking, up to `max` bytes.
fn read_text(path: &Path, max: usize) -> std::io::Result<Option<String>> {
    use std::io::{Error, ErrorKind, Read};
    use std::os::unix::fs::OpenOptionsExt;
    let nonblock = rustix::fs::OFlags::NONBLOCK.bits() as i32;
    let file = match std::fs::OpenOptions::new().read(true).custom_flags(nonblock).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("larger than {} KiB; Pulse does not load config files that size", max / 1024),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|e| Error::new(ErrorKind::InvalidData, e))
}

fn io_error(path: &Path, e: std::io::Error) -> ConfigError {
    ConfigError {
        file: Some(path.display().to_string()),
        line: None,
        column: None,
        item: None,
        field: None,
        message: format!("cannot read: {e}"),
    }
}

impl Loader {
    pub(crate) fn new(home: PulseHome, user_home: PathBuf, trust: Vec<TrustRecord>) -> Self {
        Loader { home, user_home, user_good: None, trust }
    }

    /// Directories whose files the config depends on (for the watcher).
    pub(crate) fn watched_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.home.root().to_path_buf()];
        dirs.extend(self.trust.iter().map(|t| t.dir.clone()));
        dirs.sort();
        dirs.dedup();
        dirs
    }

    fn project_status(&self, t: &TrustRecord) -> (Project, Option<String>) {
        let text = read_text(&t.file(), MAX_PROJECT_FILE).ok().flatten();
        let current = text.as_deref().map(|s| hash(s.as_bytes()));
        let state = match &current {
            None => TrustState::Missing,
            Some(h) if *h == t.hash => TrustState::Trusted,
            Some(_) => TrustState::Changed,
        };
        let project = Project {
            name: t.name.clone(),
            dir: t.dir.display().to_string(),
            file: t.file().display().to_string(),
            state,
            trusted_hash: Some(t.hash.clone()),
            current_hash: current,
            trusted_at: Some(t.trusted_at),
        };
        (project, text.filter(|_| state == TrustState::Trusted))
    }

    /// Reads every file and builds the configuration they describe.
    pub(crate) fn load(&mut self) -> Loaded {
        let mut errors = Vec::new();
        let origin = user_origin(&self.home);
        let path = self.home.config_file();
        let user = match read_text(&path, MAX_USER_FILE) {
            Ok(None) => {
                self.user_good = Some((hash(b""), ConfigFile::default()));
                ConfigFile::default()
            }
            Ok(Some(text)) => match parse_config_file(&text, &origin, &self.user_home) {
                Ok(file) => {
                    self.user_good = Some((hash(text.as_bytes()), file.clone()));
                    file
                }
                Err(e) => {
                    errors.extend(e.0);
                    self.user_good.as_ref().map(|(_, f)| f.clone()).unwrap_or_default()
                }
            },
            Err(e) => {
                errors.push(io_error(&path, e));
                self.user_good.as_ref().map(|(_, f)| f.clone()).unwrap_or_default()
            }
        };
        let mut files = vec![Sourced { origin, config: user }];
        let mut projects = Vec::new();
        for t in &self.trust {
            let (project, text) = self.project_status(t);
            projects.push(project);
            let Some(text) = text else { continue };
            match parse_config_file(&text, &t.origin(), &self.user_home) {
                Ok(file) => files.push(Sourced { origin: t.origin(), config: file }),
                Err(e) => errors.extend(e.0),
            }
        }
        let snapshot = match build(files, &self.user_home) {
            Ok(s) => Some(s),
            Err(e) => {
                errors.extend(e.0);
                None
            }
        };
        Loaded { snapshot, errors, projects }
    }

    /// The file a definition goes into: the user file, or a trusted project's file. For a
    /// project, also the trusted hash, which the text that is about to be edited must have
    /// (see [`Loader::check_trusted`]).
    pub(crate) fn target(
        &self,
        project: Option<&str>,
    ) -> ServiceResult<(ConfigOrigin, PathBuf, Option<String>)> {
        match project {
            None => Ok((user_origin(&self.home), self.home.config_file(), None)),
            Some(name) => {
                let t = self
                    .trust
                    .iter()
                    .find(|t| t.name == name)
                    .ok_or_else(|| ServiceError::not_found("trusted project", name))?;
                Ok((t.origin(), t.file(), Some(t.hash.clone())))
            }
        }
    }

    /// Refuses to edit a project file whose `text` (read once, and the very text that is then
    /// edited) is not the trusted content: writing it back would trust whatever a `git pull`
    /// put there.
    pub(crate) fn check_trusted(
        origin: &ConfigOrigin,
        trusted: Option<&str>,
        text: &str,
    ) -> ServiceResult<()> {
        match trusted {
            Some(h) if hash(text.as_bytes()) != h => {
                let dir = origin.base_dir().map(|d| d.display().to_string()).unwrap_or_default();
                Err(ServiceError::Conflict(format!(
                    "project {:?} changed since it was trusted; review it and trust it again (`pulse project trust {dir}`) first",
                    origin.project().unwrap_or_default(),
                )))
            }
            _ => Ok(()),
        }
    }

    /// Validates `text` as the new content of `origin`'s file together with the other files
    /// currently in effect (`current`), as a load would.
    pub(crate) fn check(
        &self,
        text: &str,
        origin: &ConfigOrigin,
        current: &Snapshot,
    ) -> ServiceResult<()> {
        let parsed = parse_config_file(text, origin, &self.user_home)
            .map_err(ServiceError::ConfigInvalid)?;
        let mut files = current.files.clone();
        match files.iter_mut().find(|f| f.origin == *origin) {
            Some(f) => f.config = parsed,
            None => files.push(Sourced { origin: origin.clone(), config: parsed }),
        }
        build(files, &self.user_home).map(drop).map_err(ServiceError::ConfigInvalid)
    }

    /// Re-binds a project's trust to its file's new content (after write-back).
    pub(crate) fn retrust_written(&mut self, origin: &ConfigOrigin, text: &str) -> bool {
        let Some(name) = origin.project() else { return false };
        match self.trust.iter_mut().find(|t| t.name == name) {
            Some(t) => {
                t.hash = hash(text.as_bytes());
                t.text = kept_text(text);
                true
            }
            None => false,
        }
    }

    /// The project at `dir`: its canonical directory, the name its definitions would get, and
    /// the file's text, read once.
    fn read_project(
        &self,
        dir: &Path,
        name: Option<&str>,
    ) -> ServiceResult<(PathBuf, String, String)> {
        if !dir.is_absolute() {
            return Err(ServiceError::Invalid(format!(
                "{} is not an absolute path",
                dir.display()
            )));
        }
        let dir = dir
            .canonicalize()
            .map_err(|e| ServiceError::Invalid(format!("cannot open {}: {e}", dir.display())))?;
        let name = match name {
            Some(n) => n.to_owned(),
            None => match self.trust.iter().find(|t| t.dir == dir) {
                Some(t) => t.name.clone(),
                None => dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned(),
            },
        };
        validate_name(&name)
            .map_err(|m| ServiceError::Invalid(format!("project {m}; choose one with --name")))?;
        let file = dir.join("pulse.toml");
        let text = read_text(&file, MAX_PROJECT_FILE)
            .map_err(|e| ServiceError::Invalid(format!("cannot read {}: {e}", file.display())))?
            .ok_or_else(|| ServiceError::Invalid(format!("{} does not exist", file.display())))?;
        if text.len() > MAX_PROJECT_FILE {
            return Err(ServiceError::Invalid(format!(
                "{} is larger than {} KiB; Pulse does not load project files that size",
                file.display(),
                MAX_PROJECT_FILE / 1024
            )));
        }
        Ok((dir, name, text))
    }

    /// What trusting `dir`'s `pulse.toml` would load, for the user to read first.
    pub(crate) fn review(&self, dir: &Path, name: Option<&str>) -> ServiceResult<ProjectReview> {
        let (dir, name, text) = self.read_project(dir, name)?;
        let existing = self.trust.iter().find(|t| t.dir == dir);
        let record = TrustRecord {
            name: name.clone(),
            dir: dir.clone(),
            hash: hash(text.as_bytes()),
            trusted_at: Timestamp::from_millis(0),
            text: None,
        };
        let parsed = parse_config_file(&text, &record.origin(), &self.user_home);
        let state = match existing {
            None => TrustState::Untrusted,
            Some(t) if t.hash == record.hash => TrustState::Trusted,
            Some(_) => TrustState::Changed,
        };
        let changes = match (existing, &parsed) {
            (Some(t), Ok(now)) if t.hash != record.hash => t
                .text
                .as_deref()
                .and_then(|old| toml::from_str::<ConfigFile>(old).ok())
                .map(|old| diff::changes(&old, now)),
            _ => None,
        };
        let (file, errors) = match parsed {
            Ok(f) => (f, Vec::new()),
            Err(e) => (ConfigFile::default(), e.0),
        };
        Ok(ProjectReview {
            name,
            dir: dir.display().to_string(),
            file: record.file().display().to_string(),
            state,
            hash: record.hash,
            text,
            rules: file.rules,
            agents: file.agents,
            sources: file.sources,
            errors,
            changes,
        })
    }

    /// Records trust for `dir`'s `pulse.toml`, if its content still has `expected` (the hash
    /// the user reviewed). The file is read once, and that text is what is checked, parsed
    /// and bound. It must be valid on its own.
    pub(crate) fn trust(
        &mut self,
        dir: &Path,
        name: Option<&str>,
        expected: &str,
        now: Timestamp,
    ) -> ServiceResult<Project> {
        let (dir, name, text) = self.read_project(dir, name)?;
        if let Some(other) = self.trust.iter().find(|t| t.name == name && t.dir != dir) {
            return Err(ServiceError::Conflict(format!(
                "the project name {name:?} is already used by {}",
                other.dir.display()
            )));
        }
        let current = hash(text.as_bytes());
        if current != expected {
            return Err(ServiceError::Stale {
                message: format!(
                    "{} changed since it was reviewed; review it again before trusting it",
                    dir.join("pulse.toml").display()
                ),
                current: Some(current),
            });
        }
        let record = TrustRecord {
            name,
            dir: dir.clone(),
            hash: current,
            trusted_at: now,
            text: kept_text(&text),
        };
        parse_config_file(&text, &record.origin(), &self.user_home)
            .map_err(ServiceError::ConfigInvalid)?;
        self.trust.retain(|t| t.dir != dir);
        self.trust.push(record.clone());
        Ok(self.project_status(&record).0)
    }

    pub(crate) fn untrust(&mut self, dir: &Path) -> ServiceResult<Project> {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let index = self
            .trust
            .iter()
            .position(|t| t.dir == dir)
            .ok_or_else(|| ServiceError::not_found("trusted project", dir.display()))?;
        let record = self.trust.remove(index);
        let (mut project, _) = self.project_status(&record);
        project.state = TrustState::Untrusted;
        project.trusted_hash = None;
        project.trusted_at = None;
        Ok(project)
    }
}

fn build(files: Vec<Sourced<ConfigFile>>, user_home: &Path) -> Result<Snapshot, ConfigErrors> {
    let mut iter = files.clone().into_iter();
    let user = iter.next().unwrap_or(Sourced {
        origin: ConfigOrigin::User { path: PathBuf::new() },
        config: ConfigFile::default(),
    });
    let config = resolve(user, iter.collect())?;
    let routing = Routing::compile(&config, user_home)?;
    let warnings = warnings(&config, user_home);
    Ok(Snapshot { config, routing, files, warnings })
}

/// Config that loads but cannot work: rules that can never fire ([`pulse_core::lint`]), and
/// agents used by a rule whose program is not installed or whose working directory does not
/// exist, so that every run would fail to start. Checked whenever the config is loaded, so the
/// user hears about it when they save the file, not at the first event.
pub(crate) fn warnings(config: &Config, user_home: &Path) -> Vec<ConfigError> {
    let mut out = pulse_core::lint::lint(config, user_home);
    for agent in &config.agents {
        let a = &agent.config;
        let used = config.rules.iter().any(|r| !r.config.archived && r.config.run.agent == a.name);
        if !used {
            continue;
        }
        let warn = |field: &str, message: String| ConfigError {
            file: Some(agent.origin.path().display().to_string()),
            line: None,
            column: None,
            item: Some(format!("agent {:?}", a.name)),
            field: Some(field.to_owned()),
            message,
        };
        let program = match &a.adapter {
            AdapterSettings::ClaudeCode(s) => {
                Some(("program", s.program.clone().unwrap_or_else(|| "claude".to_owned())))
            }
            AdapterSettings::Codex(s) => {
                Some(("program", s.program.clone().unwrap_or_else(|| "codex".to_owned())))
            }
            AdapterSettings::Command(s) => s.command.first().map(|p| ("command", p.clone())),
            AdapterSettings::Mcp(_) => None,
        };
        if let Some((field, program)) = program {
            if !program_exists(&program) {
                let where_ = if program.contains('/') { "" } else { " on PATH" };
                out.push(warn(
                    field,
                    format!(
                        "`{program}` was not found{where_}, so every run of this agent will fail to start"
                    ),
                ));
            }
        }
        if let Some(cwd) = &a.cwd {
            if let Ok(dir) = expand_path(cwd, user_home, agent.origin.base_dir()) {
                if !Path::new(&dir).is_dir() {
                    out.push(warn(
                        "cwd",
                        format!(
                            "the working directory {dir} does not exist, so runs of this agent will fail to start until it does"
                        ),
                    ));
                }
            }
        }
    }
    out
}

/// Whether `program` can be started: an absolute path that is a file, or a bare name found in
/// a `PATH` directory (the daemon's `PATH` is what runs inherit). Relative paths with a `/` are
/// resolved against the run's directory and are not checked.
fn program_exists(program: &str) -> bool {
    if program.starts_with('/') {
        return Path::new(program).is_file();
    }
    if program.contains('/') {
        return true;
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(program).is_file()))
}

/// What changed between two configs, for notifying clients.
pub(crate) fn changed_topics(old: &Config, new: &Config) -> Vec<pulse_contracts::rpc::StateTopic> {
    use pulse_contracts::rpc::StateTopic;
    let mut topics = vec![StateTopic::Config];
    if old.rules != new.rules {
        topics.push(StateTopic::Rules);
    }
    if old.agents != new.agents {
        topics.push(StateTopic::Agents);
    }
    if old.sources != new.sources {
        topics.push(StateTopic::Sources);
    }
    topics
}

/// Names of rules per origin file, used to keep project prefixes straight in services.
pub(crate) fn bare_name<'n>(full: &'n str, origin: &ConfigOrigin) -> &'n str {
    match origin.project() {
        Some(p) => full.strip_prefix(p).and_then(|r| r.strip_prefix('/')).unwrap_or(full),
        None => full,
    }
}

#[cfg(test)]
mod read_text_tests {
    use super::*;

    #[test]
    fn only_a_bounded_regular_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pulse.toml");
        assert_eq!(read_text(&file, 16).unwrap(), None);
        std::fs::write(&file, "0123456789abcdef").unwrap();
        assert_eq!(read_text(&file, 16).unwrap().as_deref(), Some("0123456789abcdef"));
        // Through a link, too: a linked file may be reviewed.
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read_text(&link, 16).unwrap().is_some());
        std::fs::write(&file, "0123456789abcdefg").unwrap();
        assert_eq!(read_text(&file, 16).unwrap_err().kind(), std::io::ErrorKind::InvalidData);

        // A FIFO with no writer would block an ordinary read forever; a device never ends.
        let fifo = dir.path().join("fifo.toml");
        // The `mkfifo` command: rustix has no `mknodat` on macOS.
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        assert_eq!(read_text(&fifo, 16).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        let zero = dir.path().join("zero.toml");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        assert_eq!(read_text(&zero, 16).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }
}
