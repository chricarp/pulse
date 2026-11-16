//! Why a command failed: the `--json` error object and the exit code (contract in
//! `docs/user/cli.md#json-output`).

use pulse_contracts::rpc::{ErrorCode, RpcError};
use pulse_daemon::client::ClientError;
use serde::Serialize;
use serde_json::Value;

/// A failed command. Under `--json` it is printed on stdout as `{"error": …}`; otherwise its
/// message goes to stderr.
#[derive(Debug, Serialize)]
pub struct Failure {
    /// `not_running`, `usage`, the daemon's error code as a word (`not_found`, `stale`, …), or
    /// one of the CLI's own (`not_pulse`, `connection`, `io`, `not_trusted`).
    pub kind: &'static str,
    pub message: String,
    /// The JSON-RPC error code, when the daemon refused the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// What the daemon attached (a `stale` error's current revision, a `config_invalid` error's
    /// list with file and line), or what the CLI knows more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Already reported on stderr (only `pulse mcp`, whose stdout is MCP's): print nothing and
    /// exit with this code.
    #[serde(skip)]
    pub reported: Option<u8>,
}

impl Failure {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Failure {
        Failure { kind, message: message.into(), code: None, data: None, reported: None }
    }

    /// Input the CLI itself found invalid: the same kind as when the daemon finds it.
    pub fn invalid(message: impl Into<String>) -> Failure {
        Failure::new(ErrorCode::InvalidParams.name(), message)
    }

    pub fn not_running(message: impl Into<String>) -> Failure {
        Failure::new("not_running", message)
    }

    pub fn usage(message: impl Into<String>) -> Failure {
        Failure::new("usage", message)
    }

    /// The connection to the daemon failed or closed.
    pub fn connection(message: impl Into<String>) -> Failure {
        Failure::new("connection", message)
    }

    pub fn internal(message: impl Into<String>) -> Failure {
        Failure::new(ErrorCode::Internal.name(), message)
    }

    /// Reported already (on stderr); exit with `code`.
    pub fn reported(code: u8) -> Failure {
        Failure { reported: Some(code), ..Failure::new("reported", String::new()) }
    }

    pub fn with_data(mut self, data: Value) -> Failure {
        self.data = Some(data);
        self
    }

    /// The same failure with a message that says what to do.
    pub fn with_message(mut self, message: impl Into<String>) -> Failure {
        self.message = message.into();
        self
    }

    pub fn exit_code(&self) -> u8 {
        if let Some(code) = self.reported {
            return code;
        }
        match self.kind {
            "not_running" => 3,
            "usage" => 2,
            _ => 1,
        }
    }

    pub fn is(&self, code: ErrorCode) -> bool {
        self.code == Some(code.code())
    }
}

impl From<RpcError> for Failure {
    fn from(err: RpcError) -> Failure {
        let kind = err.kind().map_or("internal", ErrorCode::name);
        // A config error list reads better one per line than as the daemon's one-line summary.
        let list = err.data.as_ref().and_then(|d| d.get("errors")).and_then(|e| {
            serde_json::from_value::<Vec<pulse_contracts::config::ConfigError>>(e.clone()).ok()
        });
        let message = match list {
            Some(list) if !list.is_empty() => {
                list.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
            }
            _ => err.message,
        };
        Failure { kind, message, code: Some(err.code), data: err.data, reported: None }
    }
}

impl From<ClientError> for Failure {
    fn from(e: ClientError) -> Failure {
        match e {
            ClientError::NotRunning(_) => Failure::not_running(e.to_string()),
            ClientError::NotPulse { .. } => Failure::new("not_pulse", e.to_string()),
            ClientError::Rpc(err) => err.into(),
            ClientError::Refused(_) | ClientError::Closed | ClientError::Protocol(_) => {
                Failure::connection(e.to_string())
            }
        }
    }
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Failure {
        Failure::new("io", e.to_string())
    }
}
