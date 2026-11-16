//! Health and status: sources, projects, the daemon as a whole.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::time::Timestamp;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Starting,
    Healthy,
    /// Working, with a problem worth showing (a watch limit hit, a calendar fetch failing and
    /// retrying).
    Degraded,
    /// Stopped by an error; will not recover without a config change or restart.
    Failed,
    Stopped,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct SourceHealth {
    pub state: SourceState,
    /// Human-readable detail for the current state.
    pub message: Option<String>,
    /// When `state` last changed.
    pub since: Timestamp,
    pub last_event_at: Option<Timestamp>,
    pub last_error: Option<SourceErrorInfo>,
    #[ts(type = "number")]
    pub events_emitted: u64,
}

impl SourceHealth {
    pub fn starting(at: Timestamp) -> Self {
        SourceHealth {
            state: SourceState::Starting,
            message: None,
            since: at,
            last_event_at: None,
            last_error: None,
            events_emitted: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SourceErrorInfo {
    pub at: Timestamp,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    /// Never trusted.
    Untrusted,
    /// Trusted, and the file still has the trusted content hash. Active.
    Trusted,
    /// Was trusted, but the file changed since. Inactive until trusted again.
    Changed,
    /// Was trusted, but the file is gone.
    Missing,
}

/// A directory with a `pulse.toml` that Pulse knows about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct Project {
    /// The prefix for its definitions (`<name>/<rule>`).
    pub name: String,
    pub dir: String,
    pub file: String,
    pub state: TrustState,
    /// SHA-256 of the trusted content, hex.
    pub trusted_hash: Option<String>,
    pub current_hash: Option<String>,
    pub trusted_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SystemStatus {
    pub version: String,
    pub started_at: Timestamp,
    /// Global pause: rules are evaluated and recorded but nothing fires.
    pub paused: bool,
    pub pulse_home: String,
    /// The user's home directory, which `~` stands for in config and in event filters. Event
    /// paths are absolute; clients use this to show them as `~/…`.
    pub user_home: String,
    pub port: u16,
    pub sources: SourceCounts,
    pub runs: RunCounts,
    pub rules: u32,
    pub agents: u32,
    /// Problems in config files; see `system.config_errors`. While the user's file has errors,
    /// its last good version stays in effect; a project file with errors is inactive.
    pub config_errors: u32,
    /// Definitions that load but cannot work as written (a rule that can never fire, an agent
    /// whose program is not installed); see `system.config_errors`.
    #[serde(default)]
    pub config_warnings: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SourceCounts {
    pub healthy: u32,
    pub degraded: u32,
    pub failed: u32,
    pub other: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RunCounts {
    pub running: u32,
    pub queued: u32,
    pub pending_approval: u32,
}

/// `$PULSE_HOME/daemon.json`, written while the daemon runs. Clients (the CLI, the desktop
/// shell) read it to find the port. It holds no secret; the token is in `$PULSE_HOME/token`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DaemonInfo {
    pub pid: u32,
    pub port: u16,
    pub version: String,
    pub started_at: Timestamp,
}

/// `POST /auth/login`: exchanges a short-lived, single-use login code (from `pulse open`, in the
/// URL fragment `#login=<code>`), or a UI resume code, for the daemon token. The whole browser handshake is described
/// in [`crate::auth`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UiLoginRequest {
    pub code: String,
}

/// `GET /auth/proof`: asks the daemon to show that it knows the credential the client is about
/// to send, before the client sends it (see [`crate::auth`] for the computation). Needs no
/// credential itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProofQuery {
    /// Chosen by the client, fresh for each attempt: 32 to 128 lowercase hex digits.
    pub nonce: String,
    /// Prove knowledge of this agent's MCP token instead of the daemon token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent: Option<String>,
    /// Prove knowledge of this daemon instance's UI resume code instead of the daemon token.
    #[serde(default)]
    pub resume: bool,
}

/// The answer to [`ProofQuery`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DaemonProof {
    /// Random per daemon start (32 hex digits). Not secret.
    pub instance: String,
    /// Lowercase hex HMAC-SHA256; see [`crate::auth`].
    pub proof: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UiLoginResponse {
    /// The daemon token. Send it as `Authorization: Bearer <token>`, or for a browser WebSocket
    /// as the subprotocols `["pulse", "pulse.token.<token>"]` (see [`crate::auth`]).
    pub token: String,
}
