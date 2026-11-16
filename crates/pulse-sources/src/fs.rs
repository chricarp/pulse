//! Filesystem watching with per-path quiet periods and storm folding into `fs.burst`.
//!
//! Stub: the constructor validates nothing beyond path expansion yet, and `run` returns
//! `NotImplemented`.

use pulse_contracts::config::FsSourceSettings;
use pulse_contracts::{SourceKind, SourceRef};

use crate::{BoxFuture, Source, SourceContext, SourceEnv, SourceError};

#[derive(Debug)]
pub struct FsSource {
    source: SourceRef,
    #[allow(dead_code)]
    settings: FsSourceSettings,
    #[allow(dead_code)]
    env: SourceEnv,
}

impl FsSource {
    pub fn new(
        name: &str,
        settings: &FsSourceSettings,
        env: &SourceEnv,
    ) -> Result<Self, SourceError> {
        Ok(FsSource {
            source: SourceRef::new(SourceKind::Fs, name),
            settings: settings.clone(),
            env: env.clone(),
        })
    }
}

impl Source for FsSource {
    fn source_ref(&self) -> &SourceRef {
        &self.source
    }

    fn run(self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            cx.health.failed("the fs source is not implemented yet");
            Err(SourceError::NotImplemented("fs"))
        })
    }
}
