//! Git activity from the reflog and repository state: commits, branch switches, conflicts, pushes.
//!
//! Stub: the constructor validates nothing beyond path expansion yet, and `run` returns
//! `NotImplemented`.

use pulse_contracts::config::GitSourceSettings;
use pulse_contracts::{SourceKind, SourceRef};

use crate::{BoxFuture, Source, SourceContext, SourceEnv, SourceError};

#[derive(Debug)]
pub struct GitSource {
    source: SourceRef,
    #[allow(dead_code)]
    settings: GitSourceSettings,
    #[allow(dead_code)]
    env: SourceEnv,
}

impl GitSource {
    pub fn new(
        name: &str,
        settings: &GitSourceSettings,
        env: &SourceEnv,
    ) -> Result<Self, SourceError> {
        Ok(GitSource {
            source: SourceRef::new(SourceKind::Git, name),
            settings: settings.clone(),
            env: env.clone(),
        })
    }
}

impl Source for GitSource {
    fn source_ref(&self) -> &SourceRef {
        &self.source
    }

    fn run(self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            cx.health.failed("the git source is not implemented yet");
            Err(SourceError::NotImplemented("git"))
        })
    }
}
