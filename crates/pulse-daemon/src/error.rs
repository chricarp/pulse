//! The error every service method returns, and its mapping onto the wire.

use pulse_contracts::config::{ConfigError, ConfigErrors};
use pulse_contracts::rpc::{ErrorCode, RpcError};
use pulse_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{what} {id:?} not found")]
    NotFound { what: &'static str, id: String },
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    ConfigInvalid(ConfigErrors),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    NotImplemented(String),
    #[error("{0}")]
    Internal(String),
}

pub type ServiceResult<T> = Result<T, ServiceError>;

impl ServiceError {
    pub(crate) fn not_found(what: &'static str, id: impl ToString) -> Self {
        ServiceError::NotFound { what, id: id.to_string() }
    }

    pub(crate) fn config(message: impl Into<String>) -> Self {
        ServiceError::ConfigInvalid(ConfigErrors(vec![ConfigError {
            file: None,
            line: None,
            column: None,
            item: None,
            field: None,
            message: message.into(),
        }]))
    }

    pub(crate) fn shutting_down() -> Self {
        ServiceError::Unavailable("the daemon is shutting down".into())
    }
}

impl From<StoreError> for ServiceError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound { what, id } => ServiceError::NotFound { what, id },
            StoreError::IllegalTransition { .. } => ServiceError::Conflict(e.to_string()),
            StoreError::InvalidQuery(m) => ServiceError::Invalid(m),
            StoreError::Closed => ServiceError::shutting_down(),
            other => ServiceError::Internal(other.to_string()),
        }
    }
}

impl From<ServiceError> for RpcError {
    fn from(e: ServiceError) -> Self {
        match e {
            ServiceError::NotFound { .. } => RpcError::new(ErrorCode::NotFound, e.to_string()),
            ServiceError::Invalid(m) => RpcError::new(ErrorCode::InvalidParams, m),
            ServiceError::Conflict(m) => RpcError::new(ErrorCode::Conflict, m),
            ServiceError::ConfigInvalid(errors) => RpcError::config_invalid(&errors),
            ServiceError::Unavailable(m) => RpcError::new(ErrorCode::Unavailable, m),
            ServiceError::NotImplemented(m) => RpcError::new(ErrorCode::NotImplemented, m),
            ServiceError::Internal(m) => RpcError::new(ErrorCode::Internal, m),
        }
    }
}
