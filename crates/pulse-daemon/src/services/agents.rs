//! `agents.*` and `sources.*`: definitions in config files, with live counts and health.

use pulse_contracts::config::{AgentConfig, SourceConfig, Sourced, validate_name};
use pulse_contracts::rpc::{
    AgentInfo, AgentList, AgentUpsertParams, McpCredentials, SourceInfo, SourceList,
    SourceUpsertParams,
};

use super::{Edit, Services};
use crate::config::{DefKind, bare_name};
use crate::error::{ServiceError, ServiceResult};

impl Services {
    async fn agent_info(&self, agent: &Sourced<AgentConfig>) -> ServiceResult<AgentInfo> {
        let name = agent.config.name.clone();
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
            Edit::Upsert(&agent),
        )
        .await?;
        self.agents_get(&full).await
    }

    /// Fails with the config errors if rules still use the agent.
    pub async fn agents_delete(&self, name: &str) -> ServiceResult<()> {
        let snapshot = self.shared.snapshot();
        let agent =
            snapshot.config.agent(name).ok_or_else(|| ServiceError::not_found("agent", name))?;
        let origin = agent.origin.clone();
        self.write_definition::<AgentConfig>(
            DefKind::Agent,
            origin.project(),
            bare_name(name, &origin),
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
            Edit::Upsert(&source),
        )
        .await?;
        let snapshot = self.shared.snapshot();
        let s = snapshot
            .config
            .source(&full)
            .ok_or_else(|| ServiceError::not_found("source", &full))?;
        Ok(SourceInfo {
            config: s.config.clone(),
            origin: s.origin.clone(),
            health: self.shared.sources.health(&full),
        })
    }

    pub async fn sources_delete(&self, name: &str) -> ServiceResult<()> {
        let snapshot = self.shared.snapshot();
        let source =
            snapshot.config.source(name).ok_or_else(|| ServiceError::not_found("source", name))?;
        let origin = source.origin.clone();
        self.write_definition::<SourceConfig>(
            DefKind::Source,
            origin.project(),
            bare_name(name, &origin),
            Edit::Delete,
        )
        .await
    }
}
