//! Parsing of external events for `POST /ingest` and `pulse ingest`: a JSON object, a JSON
//! array, or JSON lines. The result is typed `IngestEvent`s; the engine forces their type into
//! `ext.<source>.` and applies dedupe.
//!
//! Stub: returns `NotImplemented`.

use pulse_contracts::rpc::IngestEvent;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unsupported content type {0:?}: send application/json or application/x-ndjson")]
    ContentType(String),
    #[error("line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("body is {size} bytes; the limit is {limit}")]
    TooLarge { size: usize, limit: usize },
    #[error("ingest parsing is not implemented yet")]
    NotImplemented,
}

/// Limits for one ingest request.
#[derive(Clone, Copy, Debug)]
pub struct IngestLimits {
    pub max_body_bytes: usize,
    pub max_events: usize,
}

impl Default for IngestLimits {
    fn default() -> Self {
        IngestLimits { max_body_bytes: 1024 * 1024, max_events: 1000 }
    }
}

pub fn parse(
    content_type: Option<&str>,
    body: &[u8],
    limits: &IngestLimits,
) -> Result<Vec<IngestEvent>, IngestError> {
    let _ = (content_type, body, limits);
    Err(IngestError::NotImplemented)
}
