//! `events.*`: history, live subscriptions, replay, ingest and report.

use pulse_contracts::catalog::BuiltinEvent;
use pulse_contracts::config::validate_name;
use pulse_contracts::rpc::{
    EventDetail, EventFilter, EventOutcome, EventPage, EventPageWithOutcomes, EventQuery,
    EventsBatch, EventsBatchWithOutcomes, IngestParams, IngestResult, ReplayParams, ReplayResult,
    ReportParams, ReportResult, ReportedEvent,
};
use pulse_contracts::{EventId, EventType, NewEvent, SourceKind, SourceRef, SubscriptionId};
use pulse_core::filter::CompiledFilter;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::Services;
use crate::engine::Input;
use crate::error::{ServiceError, ServiceResult};
use crate::hub::{Live, Permit, Sent};

/// Events per `events.ingest` call.
pub const MAX_INGEST_EVENTS: usize = 1000;
/// The source name `pulse run` reports under: `pulse://process/run`.
pub const REPORT_SOURCE: &str = "run";

impl Services {
    /// The log, as agents may see it (no outcomes): the MCP backend's query.
    pub async fn events_query(&self, mut query: EventQuery) -> ServiceResult<EventPage> {
        expand_home(&mut query.filter, &self.shared.user_home);
        Ok(self.shared.store.read(move |q| q.query().query_events(&query)).await?)
    }

    /// The log with what each event caused, for clients holding the daemon token.
    pub async fn events_query_with_outcomes(
        &self,
        mut query: EventQuery,
    ) -> ServiceResult<EventPageWithOutcomes> {
        expand_home(&mut query.filter, &self.shared.user_home);
        Ok(self
            .shared
            .store
            .read(move |q| {
                let q = q.query();
                let page = q.query_events(&query)?;
                let ids: Vec<EventId> = page.events.iter().map(|e| e.id).collect();
                let outcomes =
                    q.deliveries_for_events(&ids)?.iter().map(EventOutcome::from).collect();
                Ok(EventPageWithOutcomes { page, outcomes })
            })
            .await?)
    }

    pub async fn events_get(&self, id: EventId) -> ServiceResult<EventDetail> {
        let detail = self
            .shared
            .store
            .read(move |q| {
                let q = q.query();
                let Some(event) = q.event(id)? else { return Ok(None) };
                Ok(Some(EventDetail {
                    event,
                    deliveries: q.deliveries_for_event(id)?,
                    runs: q.runs_for_event(id)?,
                }))
            })
            .await?;
        detail.ok_or_else(|| ServiceError::not_found("event", id))
    }

    /// Live events matching `filter`, delivered in bounded, rate-limited batches (see
    /// [`crate::hub`]). With `agent`, only events that agent's scopes cover, checked against
    /// the configuration in effect at delivery time.
    ///
    /// Without outcomes: this is the MCP backend's subscription, and agents are not told what
    /// other rules decided.
    pub(crate) fn events_subscribe(
        &self,
        filter: EventFilter,
        agent: Option<String>,
        mut deliver: impl FnMut(&EventsBatch) -> Sent + Send + 'static,
        stop: CancellationToken,
        client: CancellationToken,
    ) -> ServiceResult<SubscriptionId> {
        let permit = agent.map(|agent| {
            let shared = self.shared.clone();
            Box::new(move |e: &pulse_contracts::Event| {
                shared.snapshot().routing.agent_permits(&agent, e)
            }) as Permit
        });
        let options = Live { permit, outcomes: false, ready: CancellationToken::new() };
        options.ready.cancel();
        self.subscribe_live(filter, options, move |b| deliver(&b.batch), stop, client)
    }

    /// Live events with what each one caused, for clients holding the daemon token. Nothing is
    /// delivered before `ready` is cancelled (after the subscribe call has been answered).
    pub(crate) fn events_subscribe_with_outcomes(
        &self,
        filter: EventFilter,
        deliver: impl FnMut(&EventsBatchWithOutcomes) -> Sent + Send + 'static,
        ready: CancellationToken,
        stop: CancellationToken,
        client: CancellationToken,
    ) -> ServiceResult<SubscriptionId> {
        let options = Live { permit: None, outcomes: true, ready };
        self.subscribe_live(filter, options, deliver, stop, client)
    }

    fn subscribe_live(
        &self,
        mut filter: EventFilter,
        options: Live,
        deliver: impl FnMut(&EventsBatchWithOutcomes) -> Sent + Send + 'static,
        stop: CancellationToken,
        client: CancellationToken,
    ) -> ServiceResult<SubscriptionId> {
        expand_home(&mut filter, &self.shared.user_home);
        let compiled = CompiledFilter::compile(&filter)
            .map_err(|e| ServiceError::Invalid(format!("invalid filter: {e}")))?;
        let id = self.shared.ids.subscription();
        self.shared.hub.spawn_event_subscription(id, compiled, options, deliver, stop, client);
        Ok(id)
    }

    /// Re-emits a past event as a new one (`pulsereplayof` set, no causation, no dedupe) and
    /// routes it to all rules or only `rules`.
    pub async fn events_replay(&self, params: ReplayParams) -> ServiceResult<ReplayResult> {
        let id = params.id;
        let original = self
            .shared
            .store
            .read(move |q| q.query().event(id))
            .await?
            .ok_or_else(|| ServiceError::not_found("event", id))?;
        if let Some(rules) = &params.rules {
            let snapshot = self.shared.snapshot();
            if let Some(missing) = rules.iter().find(|r| snapshot.config.rule(r).is_none()) {
                return Err(ServiceError::not_found("rule", missing));
            }
        }
        let source = SourceRef::parse_uri(&original.source)
            .ok_or_else(|| ServiceError::Invalid(format!("event {id} has no replayable source")))?;
        let event = NewEvent {
            type_: original.type_,
            source,
            subject: original.subject,
            time: self.shared.clock.now(),
            data: original.data,
            dedupe_key: None,
            coalesced: original.coalesced,
            causation_run: None,
        };
        let (reply, rx) = oneshot::channel();
        self.shared
            .engine
            .input(Input::Replay { event, replay_of: id, rules: params.rules, reply })
            .await?;
        let (event, deliveries) = rx.await.map_err(|_| ServiceError::shutting_down())??;
        Ok(ReplayResult { event, deliveries })
    }

    async fn submit(&self, events: Vec<NewEvent>) -> ServiceResult<Vec<Option<EventId>>> {
        let (reply, rx) = oneshot::channel();
        self.shared.engine.input(Input::Ingest { events, reply }).await?;
        rx.await.map_err(|_| ServiceError::shutting_down())?
    }

    /// External events. Their types are forced into `ext.<source>.`, so ingest can never
    /// impersonate a built-in event.
    pub async fn events_ingest(&self, params: IngestParams) -> ServiceResult<IngestResult> {
        validate_name(&params.source).map_err(|m| ServiceError::Invalid(format!("source: {m}")))?;
        if params.events.len() > MAX_INGEST_EVENTS {
            return Err(ServiceError::Invalid(format!(
                "{} events in one call; the limit is {MAX_INGEST_EVENTS}",
                params.events.len()
            )));
        }
        let now = self.shared.clock.now();
        let source = SourceRef::new(SourceKind::Ext, &params.source);
        let events = params
            .events
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                let type_ = EventType::external(&params.source, &e.type_)
                    .map_err(|err| ServiceError::Invalid(format!("event {i}: {err}")))?;
                Ok(NewEvent {
                    type_,
                    source: source.clone(),
                    subject: e.subject,
                    time: e.time.unwrap_or(now),
                    data: e.data,
                    dedupe_key: e.dedupe_key,
                    coalesced: None,
                    causation_run: e.causation_run,
                })
            })
            .collect::<ServiceResult<Vec<_>>>()?;
        let ids = self.submit(events).await?;
        let duplicates = ids.iter().filter(|i| i.is_none()).count();
        Ok(IngestResult {
            accepted: ids.into_iter().flatten().collect(),
            duplicates: u32::try_from(duplicates).unwrap_or(u32::MAX),
        })
    }

    /// Built-in events from trusted local reporters (`pulse run`), with exact causation from
    /// the reporter's `PULSE_RUN_ID`.
    pub async fn events_report(&self, params: ReportParams) -> ServiceResult<ReportResult> {
        let now = self.shared.clock.now();
        let mut event = match params.event {
            ReportedEvent::ProcessExited(data) => BuiltinEvent::ProcessExited(data)
                .into_new_event(SourceRef::new(SourceKind::Process, REPORT_SOURCE), now),
        };
        event.causation_run = params.causation_run;
        let ids = self.submit(vec![event]).await?;
        Ok(ReportResult { id: ids.into_iter().next().flatten() })
    }
}

/// `~` in a filter's `path` and `subject` means the user's home directory, as in config.
fn expand_home(filter: &mut EventFilter, user_home: &std::path::Path) {
    for v in [&mut filter.path, &mut filter.subject].into_iter().flatten() {
        *v = super::expand_tilde(v, user_home);
    }
}
