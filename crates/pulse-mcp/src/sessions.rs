//! Sessions for clients on protocol revisions before 2026-07-28, bounded per agent.
//!
//! rmcp's [`LocalSessionManager`] keeps every session until its client closes it or it has been
//! idle for five minutes, with no cap. [`Sessions`] wraps it, records which agent opened each
//! session, and keeps at most [`Limits::sessions_per_agent`](crate::Limits) per agent: past
//! it, a new session closes that agent's least recently used *idle* one (no request or stream
//! open). rmcp offers no cap of its own in 3.10.0; its `SessionConfig` only sets timeouts.
//!
//! Why evict rather than refuse: a client that crashed and reconnects does not close its old
//! sessions, so refusing would lock an honest client out for up to the idle timeout, while the
//! sessions it left behind are exactly the least recently used. The cost is that a client
//! holding the same agent token can push that agent's other idle sessions out; their clients
//! get HTTP 404 on their next request, which the spec defines as "start a new session". It
//! cannot touch another agent's sessions. Refusal never happens: `sessions_per_agent` is above
//! `requests_per_agent`, so some session of the agent is always idle (and if a test configures
//! it otherwise, the least recently used busy one goes).
//!
//! `create_session` takes no request, so the agent is passed in [`CALLER`], a task-local the
//! bearer middleware sets around the request; rmcp awaits `create_session` inside that
//! request's future.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::{Stream, StreamExt};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::streamable_http_server::SessionManager;
use rmcp::transport::streamable_http_server::session::local::{
    LocalSessionManager, LocalSessionManagerError,
};
use rmcp::transport::streamable_http_server::session::{ServerSseMessage, SessionId};
use tokio::time::Instant;

use crate::AgentIdentity;

tokio::task_local! {
    /// The authenticated agent of the HTTP request being served.
    pub(crate) static CALLER: AgentIdentity;
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionsError {
    #[error(transparent)]
    Local(#[from] LocalSessionManagerError),
    /// Unreachable behind the bearer middleware; fail closed if it was not applied.
    #[error("no authenticated agent for this session")]
    Unauthenticated,
}

pub(crate) struct Sessions {
    inner: LocalSessionManager,
    per_agent: usize,
    owners: Arc<Mutex<HashMap<SessionId, Owner>>>,
}

struct Owner {
    agent: AgentIdentity,
    last_used: Instant,
    /// Requests and streams open on the session.
    open: u32,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Sessions {
    pub fn new(per_agent: usize) -> Self {
        Sessions { inner: LocalSessionManager::default(), per_agent, owners: Arc::default() }
    }

    /// Marks the session used and returns a guard that counts it as busy until dropped.
    fn open(&self, id: &SessionId) -> Open {
        if let Some(o) = lock(&self.owners).get_mut(id) {
            o.open += 1;
            o.last_used = Instant::now();
        }
        Open { owners: self.owners.clone(), id: id.clone() }
    }

    /// Sessions of `agent` to close so that it keeps at most `per_agent`, keeping `new`.
    fn over_cap(&self, agent: &AgentIdentity, new: &SessionId) -> Vec<SessionId> {
        let mut owners = lock(&self.owners);
        let mut theirs: Vec<(&SessionId, &Owner)> =
            owners.iter().filter(|(id, o)| o.agent == *agent && *id != new).collect();
        // Idle first, then least recently used.
        theirs.sort_by_key(|(_, o)| (o.open > 0, o.last_used));
        let excess = (theirs.len() + 1).saturating_sub(self.per_agent);
        let victims: Vec<SessionId> =
            theirs.iter().take(excess).map(|(id, _)| (*id).clone()).collect();
        for v in &victims {
            owners.remove(v);
        }
        victims
    }
}

/// Counts a request or stream on a session until dropped.
struct Open {
    owners: Arc<Mutex<HashMap<SessionId, Owner>>>,
    id: SessionId,
}

impl Drop for Open {
    fn drop(&mut self) {
        if let Some(o) = lock(&self.owners).get_mut(&self.id) {
            o.open = o.open.saturating_sub(1);
            o.last_used = Instant::now();
        }
    }
}

/// `stream`, counted as open on its session until it ends or is dropped.
fn counted(
    stream: impl Stream<Item = ServerSseMessage> + Send + Sync + 'static,
    open: Open,
) -> impl Stream<Item = ServerSseMessage> + Send + Sync + 'static {
    stream.map(move |m| {
        let _counted = &open;
        m
    })
}

impl SessionManager for Sessions {
    type Error = SessionsError;
    type Transport = <LocalSessionManager as SessionManager>::Transport;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        let agent = CALLER.try_with(Clone::clone).map_err(|_| SessionsError::Unauthenticated)?;
        let (id, transport) = self.inner.create_session().await?;
        lock(&self.owners)
            .insert(id.clone(), Owner { agent: agent.clone(), last_used: Instant::now(), open: 0 });
        // Counted and chosen under one lock, so concurrent `initialize`s cannot overshoot.
        for victim in self.over_cap(&agent, &id) {
            close_evicted(&self.inner, &victim).await;
        }
        Ok((id, transport))
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        let _open = self.open(id);
        Ok(self.inner.initialize_session(id, message).await?)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        Ok(self.inner.has_session(id).await?)
    }

    /// Called for `DELETE`, and by rmcp when a session's worker ends (idle timeout, shutdown,
    /// or after we closed it here).
    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        lock(&self.owners).remove(id);
        Ok(self.inner.close_session(id).await?)
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        let open = self.open(id);
        Ok(counted(self.inner.create_stream(id, message).await?, open))
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        let _open = self.open(id);
        Ok(self.inner.accept_message(id, message).await?)
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        let open = self.open(id);
        Ok(counted(self.inner.create_standalone_stream(id).await?, open))
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        let open = self.open(id);
        Ok(counted(self.inner.resume(id, last_event_id).await?, open))
    }
    // `restore_session` keeps the default (not supported): it would create sessions outside
    // `create_session`, and Pulse configures no external session store.
}

/// Closes an evicted session. Its client finds out on its next request (404).
async fn close_evicted(inner: &LocalSessionManager, id: &SessionId) {
    // The only error is a session whose worker already ended, which is the goal anyway.
    let _ = inner.close_session(id).await;
}
