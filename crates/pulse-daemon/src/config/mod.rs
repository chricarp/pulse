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

pub(crate) mod edit;
pub(crate) mod watch;

use std::path::{Path, PathBuf};

use pulse_contracts::config::{
    Config, ConfigError, ConfigErrors, ConfigFile, ConfigOrigin, RuleConfig, Sourced,
    parse_config_file, resolve, validate_file, validate_name,
};
use pulse_contracts::status::{Project, TrustState};
use pulse_contracts::Timestamp;
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
        let routing =
            Routing::compile(&config, user_home).map_err(ServiceError::ConfigInvalid)?;
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

fn read_text(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
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
        let text = read_text(&t.file()).ok().flatten();
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
        let user = match read_text(&path) {
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

    /// Whether the user file's text differs from what was last loaded.
    pub(crate) fn user_file_changed(&self) -> bool {
        let current = read_text(&self.home.config_file()).ok().flatten().unwrap_or_default();
        self.user_good.as_ref().is_none_or(|(h, _)| *h != hash(current.as_bytes()))
    }

    /// The file a definition goes into: the user file, or a project's file while it is trusted.
    pub(crate) fn target(&self, project: Option<&str>) -> ServiceResult<(ConfigOrigin, PathBuf)> {
        match project {
            None => Ok((user_origin(&self.home), self.home.config_file())),
            Some(name) => {
                let t = self
                    .trust
                    .iter()
                    .find(|t| t.name == name)
                    .ok_or_else(|| ServiceError::not_found("trusted project", name))?;
                if self.project_status(t).0.state != TrustState::Trusted {
                    return Err(ServiceError::Conflict(format!(
                        "project {name:?} changed since it was trusted; review it and run `pulse project trust {}` first",
                        t.dir.display()
                    )));
                }
                Ok((t.origin(), t.file()))
            }
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
                true
            }
            None => false,
        }
    }

    /// Records trust for `dir`'s `pulse.toml` as it is now. The file must be valid on its own.
    pub(crate) fn trust(
        &mut self,
        dir: &Path,
        name: Option<&str>,
        now: Timestamp,
    ) -> ServiceResult<Project> {
        if !dir.is_absolute() {
            return Err(ServiceError::Invalid(format!(
                "{} is not an absolute path",
                dir.display()
            )));
        }
        let dir = dir.canonicalize().map_err(|e| {
            ServiceError::Invalid(format!("cannot open {}: {e}", dir.display()))
        })?;
        let name = match name {
            Some(n) => n.to_owned(),
            None => dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned(),
        };
        validate_name(&name).map_err(|m| {
            ServiceError::Invalid(format!("project {m}; choose one with --name"))
        })?;
        if let Some(other) = self.trust.iter().find(|t| t.name == name && t.dir != dir) {
            return Err(ServiceError::Conflict(format!(
                "the project name {name:?} is already used by {}",
                other.dir.display()
            )));
        }
        let file = dir.join("pulse.toml");
        let text = read_text(&file)
            .map_err(|e| ServiceError::Invalid(format!("cannot read {}: {e}", file.display())))?
            .ok_or_else(|| ServiceError::Invalid(format!("{} does not exist", file.display())))?;
        let record = TrustRecord { name, dir: dir.clone(), hash: hash(text.as_bytes()), trusted_at: now };
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
    Ok(Snapshot { config, routing, files })
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
