//! JSON-RPC 2.0 over WebSocket: framing, errors, the v1 method set and server notifications.
//!
//! The method table in [`methods`] is the single definition. From it come:
//! - [`Request`]: every method with typed params; [`Request::parse`] decodes a frame.
//! - [`RpcHandler`] and [`dispatch`]: the daemon implements one trait method per RPC method, so
//!   a missing method is a compile error, and results are typed.
//! - [`RpcMethod`] marker types in [`method`] for a typed client call:
//!   `client.call::<method::RunsApprove>(RunIdParams { id })`.
//! - The TypeScript `RpcMethods` map (see [`crate::typescript`]).

mod methods;

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub use methods::*;

/// The `jsonrpc` member. Only `"2.0"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum JsonRpcVersion {
    #[default]
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// A request from a client. A request without `id` is a notification and gets no response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct JsonRpcRequest {
    pub jsonrpc: JsonRpcVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    #[ts(type = "unknown")]
    pub params: Value,
}

/// A response. Exactly one of `result` and `error` is present.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct JsonRpcResponse {
    pub jsonrpc: JsonRpcVersion,
    /// `null` when the request could not be read far enough to know its id.
    pub id: Option<RequestId>,
    #[serde(flatten)]
    pub payload: ResponsePayload,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ResponsePayload {
    Result(#[ts(type = "unknown")] Value),
    Error(RpcError),
}

impl JsonRpcResponse {
    pub fn ok(id: Option<RequestId>, result: Value) -> Self {
        JsonRpcResponse {
            jsonrpc: JsonRpcVersion::V2,
            id,
            payload: ResponsePayload::Result(result),
        }
    }
    pub fn err(id: Option<RequestId>, error: RpcError) -> Self {
        JsonRpcResponse { jsonrpc: JsonRpcVersion::V2, id, payload: ResponsePayload::Error(error) }
    }
}

/// A server-to-client notification frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct JsonRpcNotification {
    pub jsonrpc: JsonRpcVersion,
    #[serde(flatten)]
    pub notification: Notification,
}

impl From<Notification> for JsonRpcNotification {
    fn from(notification: Notification) -> Self {
        JsonRpcNotification { jsonrpc: JsonRpcVersion::V2, notification }
    }
}

/// Error codes. The first five are JSON-RPC's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    Internal,
    /// Authentication failed or the token does not allow this.
    Unauthorized,
    NotFound,
    /// The request conflicts with current state (an illegal run transition, a name in use).
    Conflict,
    /// A definition did not validate. `data` is `{ "errors": ConfigError[] }`.
    ConfigInvalid,
    /// The daemon is shutting down or a dependency is down.
    Unavailable,
    NotImplemented,
    /// What the request was based on is no longer so: a definition changed since the client
    /// read it (or a name it meant to create now exists), or a project file no longer has the
    /// content hash the client reviewed. `data` is a [`Stale`]. Reload, show the user what
    /// changed, and let them decide.
    Stale,
}

impl ErrorCode {
    pub const fn code(self) -> i32 {
        match self {
            ErrorCode::ParseError => -32700,
            ErrorCode::InvalidRequest => -32600,
            ErrorCode::MethodNotFound => -32601,
            ErrorCode::InvalidParams => -32602,
            ErrorCode::Internal => -32603,
            ErrorCode::Unauthorized => -32001,
            ErrorCode::NotFound => -32002,
            ErrorCode::Conflict => -32003,
            ErrorCode::ConfigInvalid => -32004,
            ErrorCode::Unavailable => -32005,
            ErrorCode::NotImplemented => -32006,
            ErrorCode::Stale => -32007,
        }
    }

    pub fn from_code(code: i32) -> Option<ErrorCode> {
        use ErrorCode::*;
        [
            ParseError,
            InvalidRequest,
            MethodNotFound,
            InvalidParams,
            Internal,
            Unauthorized,
            NotFound,
            Conflict,
            ConfigInvalid,
            Unavailable,
            NotImplemented,
            Stale,
        ]
        .into_iter()
        .find(|c| c.code() == code)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(optional_fields)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown")]
    pub data: Option<Value>,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        RpcError { code: code.code(), message: message.into(), data: None }
    }
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
    pub fn kind(&self) -> Option<ErrorCode> {
        ErrorCode::from_code(self.code)
    }
    pub fn method_not_found(method: &str) -> Self {
        RpcError::new(ErrorCode::MethodNotFound, format!("unknown method {method:?}"))
    }
    pub fn invalid_params(err: impl fmt::Display) -> Self {
        RpcError::new(ErrorCode::InvalidParams, format!("invalid params: {err}"))
    }
    pub fn not_found(what: &str, id: impl fmt::Display) -> Self {
        RpcError::new(ErrorCode::NotFound, format!("{what} {id} not found"))
    }
    pub fn config_invalid(errors: &crate::config::ConfigErrors) -> Self {
        RpcError::new(ErrorCode::ConfigInvalid, errors.to_string())
            .with_data(serde_json::json!({ "errors": errors.0 }))
    }
    pub fn stale(message: impl Into<String>, data: &Stale) -> Self {
        RpcError::new(ErrorCode::Stale, message)
            .with_data(serde_json::to_value(data).unwrap_or(Value::Null))
    }
    pub fn not_implemented(what: &str) -> Self {
        RpcError::new(ErrorCode::NotImplemented, format!("{what} is not implemented yet"))
    }
}

/// One RPC method, for typed clients.
pub trait RpcMethod {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned + Send + 'static;
    type Result: Serialize + DeserializeOwned + Send + 'static;
}

/// Builds the request frame for `M`.
pub fn request<M: RpcMethod>(
    id: RequestId,
    params: &M::Params,
) -> Result<JsonRpcRequest, serde_json::Error> {
    Ok(JsonRpcRequest {
        jsonrpc: JsonRpcVersion::V2,
        id: Some(id),
        method: M::NAME.to_owned(),
        params: serde_json::to_value(params)?,
    })
}

/// Reads the response to a request for `M`.
pub fn decode_response<M: RpcMethod>(resp: JsonRpcResponse) -> Result<M::Result, RpcError> {
    match resp.payload {
        ResponsePayload::Result(v) => serde_json::from_value(v).map_err(|e| {
            RpcError::new(ErrorCode::Internal, format!("unexpected result for {}: {e}", M::NAME))
        }),
        ResponsePayload::Error(e) => Err(e),
    }
}

/// The TypeScript constants for error codes clients branch on.
pub(crate) fn typescript_constants() -> String {
    format!(
        "/** The `code` of a `stale` RPC error; its `data` is a `Stale`. */\nexport const ERROR_STALE = {};\n",
        ErrorCode::Stale.code()
    )
}

fn decode_params<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    // Methods without parameters accept a missing, `null` or `{}` params member.
    let params = if params.is_null() { Value::Object(Default::default()) } else { params };
    serde_json::from_value(params).map_err(RpcError::invalid_params)
}

fn encode_result<T: Serialize>(result: T) -> Result<Value, RpcError> {
    serde_json::to_value(result).map_err(|e| RpcError::new(ErrorCode::Internal, e.to_string()))
}

#[cfg(test)]
mod tests;
