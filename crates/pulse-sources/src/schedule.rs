//! `schedule.fired` on a cron expression or a fixed interval. Arms one timer for the next fire.
//!
//! Stub: the constructor validates nothing beyond path expansion yet, and `run` returns
//! `NotImplemented`.

use pulse_contracts::config::ScheduleSourceSettings;
use pulse_contracts::{SourceKind, SourceRef};

use crate::{BoxFuture, Source, SourceContext, SourceEnv, SourceError};

#[derive(Debug)]
pub struct ScheduleSource {
    source: SourceRef,
    #[allow(dead_code)]
    settings: ScheduleSourceSettings,
    #[allow(dead_code)]
    env: SourceEnv,
}

impl ScheduleSource {
    pub fn new(
        name: &str,
        settings: &ScheduleSourceSettings,
        env: &SourceEnv,
    ) -> Result<Self, SourceError> {
        Ok(ScheduleSource {
            source: SourceRef::new(SourceKind::Schedule, name),
            settings: settings.clone(),
            env: env.clone(),
        })
    }
}

impl Source for ScheduleSource {
    fn source_ref(&self) -> &SourceRef {
        &self.source
    }

    fn run(self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            cx.health.failed("the schedule source is not implemented yet");
            Err(SourceError::NotImplemented("schedule"))
        })
    }
}
