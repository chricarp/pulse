//! `agents.*` and `sources.*`: definitions in config files, with live counts and health.

use pulse_contracts::config::{AgentConfig, SourceConfig, Sourced, validate_name};
use pulse_contracts::rpc::{
    AgentInfo, AgentList, AgentUpsertParams, McpCredentials, SourceInfo, SourceList,
    SourceUpsertParams,
};

use pulse_contracts::config::ConfigOrigin;
use pulse_contracts::rpc::Expected;

use super::{Edit, Services, written_revision};
use crate::config::{DefKind, Snapshot, bare_name};

fn agent_revision(snapshot: &Snapshot, a: &Sourced<AgentConfig>) -> String {
    let bare = bare_name(&a.config.name, &a.origin);
    written_revision(&snapshot.files, &a.origin, |f| f.agents.iter().find(|x| x.name == bare))
}

fn source_revision(snapshot: &Snapshot, s: &Sourced<SourceConfig>) -> String {
    let bare = bare_name(&s.config.name, &s.origin);
    written_revision(&snapshot.files, &s.origin, |f| f.sources.iter().find(|x| x.name == bare))
}
use crate::error::{ServiceError, ServiceResult};

impl Services {
    async fn agent_info(&self, agent: &Sourced<AgentConfig>) -> ServiceResult<AgentInfo> {
        let name = agent.config.name.clone();
        let revision = agent_revision(&self.shared.snapshot(), agent);
        let backlog = self
            .shared
            .store
            .read(|q| q.query().agent_backlog())
            .await?
            .get(&name)
            .copied()
            .unwrap_or(0);
        let running = self.shared.running(&name);
        Ok(AgentInfo {
            config: agent.config.clone(),
            origin: agent.origin.clone(),
            revision,
            running,
            queued: backlog.saturating_sub(running),
        })
    }

    pub async fn agents_list(&self) -> ServiceResult<AgentList> {
        let snapshot = self.shared.snapshot();
        let backlog = self.shared.store.read(|q| q.query().agent_backlog()).await?;
        let agents = snapshot
            .config
            .agents
            .iter()
            .map(|a| {
                let running = self.shared.running(&a.config.name);
                let total = backlog.get(&a.config.name).copied().unwrap_or(0);
                AgentInfo {
                    config: a.config.clone(),
                    origin: a.origin.clone(),
                    revision: agent_revision(&snapshot, a),
                    running,
                    queued: total.saturating_sub(running),
                }
            })
            .collect();
        Ok(AgentList { agents })
    }

    pub async fn agents_get(&self, name: &str) -> ServiceResult<AgentInfo> {
        let snapshot = self.shared.snapshot();
        let agent =
            snapshot.config.agent(name).ok_or_else(|| ServiceError::not_found("agent", name))?;
        self.agent_info(agent).await
    }

    pub async fn agents_upsert(&self, params: AgentUpsertParams) -> ServiceResult<AgentInfo> {
        let agent = params.agent;
        validate_name(&agent.name).map_err(ServiceError::Invalid)?;
        let full = match &params.project {
            Some(p) => format!("{p}/{}", agent.name),
            None => agent.name.clone(),
        };
        self.write_definition(
            DefKind::Agent,
            params.project.as_deref(),
            &agent.name,
            &params.expected,
            Edit::Put(&agent),
        )
        .await?;
        self.agents_get(&full).await
    }

    /// Fails with the config errors if rules still use the agent.
    pub async fn agents_delete(&self, name: &str, expected: &Expected) -> ServiceResult<()> {
        let origin = self.origin_of(DefKind::Agent, name, expected)?;
        self.write_definition::<AgentConfig>(
            DefKind::Agent,
            origin.project(),
            bare_name(name, &origin),
            expected,
            Edit::Delete,
        )
        .await
    }

    /// The `/mcp` URL and the agent's derived token.
    pub async fn agents_mcp_credentials(&self, name: &str) -> ServiceResult<McpCredentials> {
        if self.shared.snapshot().config.agent(name).is_none() {
            return Err(ServiceError::not_found("agent", name));
        }
        Ok(McpCredentials {
            agent: name.to_owned(),
            url: format!("http://127.0.0.1:{}/mcp", self.shared.port()),
            token: crate::auth::agent_token(&self.shared.secret, name),
        })
    }

    pub async fn sources_list(&self) -> ServiceResult<SourceList> {
        let snapshot = self.shared.snapshot();
        let sources = snapshot
            .config
            .sources
            .iter()
            .map(|s| SourceInfo {
                config: s.config.clone(),
                origin: s.origin.clone(),
                revision: source_revision(&snapshot, s),
                health: self.shared.sources.health(&s.config.name),
            })
            .collect();
        Ok(SourceList { sources })
    }

    pub async fn sources_upsert(&self, params: SourceUpsertParams) -> ServiceResult<SourceInfo> {
        let source = params.source;
        validate_name(&source.name).map_err(ServiceError::Invalid)?;
        let full = match &params.project {
            Some(p) => format!("{p}/{}", source.name),
            None => source.name.clone(),
        };
        self.write_definition(
            DefKind::Source,
            params.project.as_deref(),
            &source.name,
            &params.expected,
            Edit::Put(&source),
        )
        .await?;
        self.sources_get(&full)
    }

    fn sources_get(&self, name: &str) -> ServiceResult<SourceInfo> {
        let snapshot = self.shared.snapshot();
        let s =
            snapshot.config.source(name).ok_or_else(|| ServiceError::not_found("source", name))?;
        Ok(SourceInfo {
            config: s.config.clone(),
            origin: s.origin.clone(),
            revision: source_revision(&snapshot, s),
            health: self.shared.sources.health(name),
        })
    }

    /// Changes `enabled` of the source as it is in its file, nothing else.
    pub async fn sources_set_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> ServiceResult<SourceInfo> {
        let origin = self.origin_of(DefKind::Source, name, &Expected::Any)?;
        let set = move |s: &mut SourceConfig| s.enabled = enabled;
        self.write_definition(
            DefKind::Source,
            origin.project(),
            bare_name(name, &origin),
            &Expected::Any,
            Edit::Modify(&set),
        )
        .await?;
        self.sources_get(name)
    }

    pub async fn sources_delete(&self, name: &str, expected: &Expected) -> ServiceResult<()> {
        let origin = self.origin_of(DefKind::Source, name, expected)?;
        self.write_definition::<SourceConfig>(
            DefKind::Source,
            origin.project(),
            bare_name(name, &origin),
            expected,
            Edit::Delete,
        )
        .await
    }

    /// The file a definition known by its full name lives in. A name the daemon does not know
    /// is `not_found`, or `stale` when the client expected a revision of it (it was removed
    /// since the client read it).
    pub(crate) fn origin_of(
        &self,
        kind: DefKind,
        name: &str,
        expected: &Expected,
    ) -> ServiceResult<ConfigOrigin> {
        if matches!(expected, Expected::Absent) {
            return Err(ServiceError::Invalid(format!(
                "`expected` cannot be `absent` for an existing {}",
                kind.array()
            )));
        }
        let config = &self.shared.snapshot().config;
        let origin = match kind {
            DefKind::Rule => config.rule(name).map(|d| d.origin.clone()),
            DefKind::Agent => config.agent(name).map(|d| d.origin.clone()),
            DefKind::Source => config.source(name).map(|d| d.origin.clone()),
        };
        origin.ok_or_else(|| match expected {
            Expected::Revision { .. } => ServiceError::Stale {
                message: format!("the {} {name:?} was removed since it was read", kind.array()),
                current: None,
            },
            _ => ServiceError::not_found(kind.array(), name),
        })
    }
}
