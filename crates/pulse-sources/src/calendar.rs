//! ICS calendars (file or URL): `calendar.event.starting` and `.ended`.
//!
//! Stub: the constructor validates nothing beyond path expansion yet, and `run` returns
//! `NotImplemented`.

use pulse_contracts::config::CalendarSourceSettings;
use pulse_contracts::{SourceKind, SourceRef};

use crate::{BoxFuture, Source, SourceContext, SourceEnv, SourceError};

#[derive(Debug)]
pub struct CalendarSource {
    source: SourceRef,
    #[allow(dead_code)]
    settings: CalendarSourceSettings,
    #[allow(dead_code)]
    env: SourceEnv,
}

impl CalendarSource {
    pub fn new(
        name: &str,
        settings: &CalendarSourceSettings,
        env: &SourceEnv,
    ) -> Result<Self, SourceError> {
        Ok(CalendarSource {
            source: SourceRef::new(SourceKind::Calendar, name),
            settings: settings.clone(),
            env: env.clone(),
        })
    }
}

impl Source for CalendarSource {
    fn source_ref(&self) -> &SourceRef {
        &self.source
    }

    fn run(self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            cx.health.failed("the calendar source is not implemented yet");
            Err(SourceError::NotImplemented("calendar"))
        })
    }
}
