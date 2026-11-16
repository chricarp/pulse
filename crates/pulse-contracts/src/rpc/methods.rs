//! The v1 method set: params and results, then the table that ties names to types.

use std::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::{RpcError, RpcMethod, decode_params, encode_result};
use crate::catalog::{EventTypeInfo, ProcessExitedData};
use crate::config::{AgentConfig, ConfigError, ConfigOrigin, RuleConfig, SourceConfig, TypeGlob};
use crate::event::Event;
use crate::ids::{DeliveryId, EventId, RunId, SubscriptionId};
use crate::run::{Decision, Delivery, OutputStats, Run, RunOutputChunk, RunStatus, RunSummary};
use crate::status::{Project, SourceHealth, SystemStatus};
use crate::time::Timestamp;

/// Page size used when a request does not give one.
pub const DEFAULT_PAGE_LIMIT: u32 = 100;
/// Larger `limit`s are clamped to this.
pub const MAX_PAGE_LIMIT: u32 = 500;

/// Clamps a requested page size.
pub fn page_limit(requested: Option<u32>) -> u32 {
    requested.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

/// No parameters, or no result. Accepts `{}`, `null` or a missing member.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

impl TS for Empty {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;
    fn name(_: &ts_rs::Config) -> String {
        "Empty".to_owned()
    }
    fn inline(_: &ts_rs::Config) -> String {
        "Record<string, never>".to_owned()
    }
    fn decl(_: &ts_rs::Config) -> String {
        "type Empty = Record<string, never>;".to_owned()
    }
    fn decl_concrete(cfg: &ts_rs::Config) -> String {
        Self::decl(cfg)
    }
    fn output_path() -> Option<std::path::PathBuf> {
        Some("Empty.ts".into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NameParams {
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RunIdParams {
    pub id: RunId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Subscribed {
    pub subscription: SubscriptionId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Unsubscribe {
    pub subscription: SubscriptionId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    #[default]
    NewestFirst,
    OldestFirst,
}

// ---- events ----

/// Which events. Every given criterion must hold; empty lists mean "any".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
#[ts(optional_fields)]
pub struct EventFilter {
    pub types: Vec<TypeGlob>,
    /// Source URIs (`pulse://fs/university`) or bare source names (`university`).
    pub sources: Vec<String>,
    /// A glob over `subject`.
    pub subject: Option<String>,
    /// Events carrying a path equal to or under this absolute directory (or equal to this file).
    pub path: Option<String>,
    /// Case-insensitive substring of the subject or the data.
    pub text: Option<String>,
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
    /// Events attributed to this run.
    pub caused_by: Option<RunId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
#[ts(optional_fields)]
pub struct EventQuery {
    pub filter: EventFilter,
    /// The `next_cursor` of the previous page.
    pub cursor: Option<EventId>,
    pub limit: Option<u32>,
    pub order: Order,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct EventPage {
    pub events: Vec<Event>,
    /// Pass as `cursor` to continue; `None` at the end.
    pub next_cursor: Option<EventId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EventIdParams {
    pub id: EventId,
}

/// An event with everything that happened because of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct EventDetail {
    pub event: Event,
    pub deliveries: Vec<Delivery>,
    pub runs: Vec<RunSummary>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
pub struct EventsSubscribeParams {
    pub filter: EventFilter,
}

/// Re-emits a past event as a new event (`pulsereplayof` set, no causation) and routes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct ReplayParams {
    pub id: EventId,
    /// Only route to these rules. Default: all rules.
    #[serde(default)]
    pub rules: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReplayResult {
    pub event: Event,
    pub deliveries: Vec<Delivery>,
}

/// One external event. Its type is always forced into `ext.<source>.`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct IngestEvent {
    /// `build.failed` or `ext.<source>.build.failed`.
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub subject: Option<String>,
    /// Default: when the daemon received it.
    #[serde(default)]
    pub time: Option<Timestamp>,
    #[serde(default)]
    #[ts(type = "unknown")]
    pub data: Value,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    /// The sender's `PULSE_RUN_ID`, if it was started by a run.
    #[serde(default)]
    pub causation_run: Option<RunId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct IngestParams {
    /// The external source name; events become `ext.<source>.<type>`.
    pub source: String,
    pub events: Vec<IngestEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct IngestResult {
    pub accepted: Vec<EventId>,
    /// Dropped because their `dedupe_key` was seen within the dedupe TTL.
    pub duplicates: u32,
}

/// Built-in events that trusted local reporters (the `pulse` CLI) may emit. Requires the daemon
/// token, like every RPC method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type")]
pub enum ReportedEvent {
    #[serde(rename = "process.exited")]
    ProcessExited(ProcessExitedData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct ReportParams {
    pub event: ReportedEvent,
    /// The reporter's `PULSE_RUN_ID`.
    #[serde(default)]
    pub causation_run: Option<RunId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct ReportResult {
    /// `None` if the event was deduplicated.
    pub id: Option<EventId>,
}

// ---- rules ----

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
pub struct RulesListParams {
    pub include_archived: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RuleList {
    pub rules: Vec<RuleInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RuleInfo {
    /// `config.name` is the full name (`project/rule` for project rules).
    pub config: RuleConfig,
    pub origin: ConfigOrigin,
    pub state: RuleRuntimeStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct RuleRuntimeStatus {
    pub snoozed_until: Option<Timestamp>,
    /// Set while the circuit breaker holds the rule. `rules.unsnooze` clears it too.
    pub circuit_open_until: Option<Timestamp>,
    pub last_fired_at: Option<Timestamp>,
}

/// Creates or replaces a rule in the user's file, or in a trusted project's file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct RuleUpsertParams {
    /// `rule.name` is the bare name, without a project prefix.
    pub rule: RuleConfig,
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SetEnabledParams {
    pub name: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SnoozeParams {
    pub name: String,
    pub until: Timestamp,
}

/// "Test this rule against a past event": runs routing on a copy of the state, with no side
/// effects, and returns what would happen and the prompt that would be sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RuleTestParams {
    pub rule: RuleRef,
    pub event: EventId,
    /// Evaluate as if the rule had no history: no snooze, cooldown, rate or debounce state.
    #[serde(default)]
    pub ignore_runtime_state: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields)]
pub enum RuleRef {
    Saved {
        name: String,
    },
    /// An unsaved rule from the editor.
    Draft {
        rule: Box<RuleConfig>,
        #[serde(default)]
        project: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct RuleTestResult {
    /// The event's type and `where` match the rule.
    pub matched: bool,
    /// What routing would decide; `None` when it does not match.
    pub decision: Option<Decision>,
    /// When `decision` is `debounced`: what would happen once the debounce elapses (including
    /// the rendered prompt).
    #[serde(default)]
    pub after_debounce: Option<Decision>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    Fired,
    AwaitingApproval,
    Debounced,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct RuleHistoryParams {
    pub name: String,
    /// Only these outcomes. Default: all.
    #[serde(default)]
    pub outcomes: Vec<DeliveryKind>,
    #[serde(default)]
    pub cursor: Option<DeliveryId>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct DeliveryPage {
    /// Newest first.
    pub deliveries: Vec<Delivery>,
    pub next_cursor: Option<DeliveryId>,
}

// ---- agents ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct AgentList {
    pub agents: Vec<AgentInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct AgentInfo {
    pub config: AgentConfig,
    pub origin: ConfigOrigin,
    pub running: u32,
    pub queued: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct AgentUpsertParams {
    pub agent: AgentConfig,
    #[serde(default)]
    pub project: Option<String>,
}

/// What an MCP client needs to act as an agent. The token is derived from the daemon secret and
/// the agent name; it is valid only on `/mcp` and only as this agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpCredentials {
    pub agent: String,
    pub url: String,
    pub token: String,
}

// ---- sources ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct SourceList {
    pub sources: Vec<SourceInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct SourceInfo {
    pub config: SourceConfig,
    pub origin: ConfigOrigin,
    pub health: SourceHealth,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct SourceUpsertParams {
    pub source: SourceConfig,
    #[serde(default)]
    pub project: Option<String>,
}

// ---- runs ----

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
#[ts(optional_fields)]
pub struct RunFilter {
    pub rule: Option<String>,
    pub agent: Option<String>,
    /// Any of these. Empty: all.
    pub status: Vec<RunStatus>,
    pub event: Option<EventId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
#[ts(optional_fields)]
pub struct RunsListParams {
    pub filter: RunFilter,
    pub cursor: Option<RunId>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct RunPage {
    /// Newest first.
    pub runs: Vec<RunSummary>,
    pub next_cursor: Option<RunId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct RunOutputParams {
    pub id: RunId,
    /// Chunks with `seq` greater than this. Default: from the start.
    #[serde(default)]
    #[ts(type = "number")]
    pub after: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct RunOutputPage {
    pub chunks: Vec<RunOutputChunk>,
    /// Pass as `after` to continue; `None` when there is nothing more stored yet.
    #[ts(type = "number | null")]
    pub next_after: Option<u64>,
    pub stats: OutputStats,
    /// The run is terminal, so no more output will come.
    pub done: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, deny_unknown_fields)]
pub struct RunsSubscribeParams {
    pub filter: RunFilter,
}

/// Streams a run's output: first stored chunks after `after`, then live ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct RunOutputSubscribeParams {
    pub id: RunId,
    #[serde(default)]
    #[ts(type = "number")]
    pub after: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct RejectParams {
    pub id: RunId,
    #[serde(default)]
    pub reason: Option<String>,
}

// ---- projects ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectList {
    pub projects: Vec<Project>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct ProjectTrustParams {
    /// The directory containing `pulse.toml`.
    pub dir: String,
    /// The prefix for its definitions. Default: the directory's name.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectDirParams {
    pub dir: String,
}

// ---- system ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct EventTypeList {
    pub types: Vec<EventTypeInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ConfigErrorList {
    /// Problems that keep a file (or the whole config) from loading.
    pub errors: Vec<ConfigError>,
    /// Definitions in effect that cannot work as written: a rule that can never fire, an agent
    /// whose program or working directory does not exist.
    #[serde(default)]
    pub warnings: Vec<ConfigError>,
}

// ---- notifications ----

/// Server-to-client notifications. Each is a JSON-RPC notification whose `method` is the tag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "method", content = "params")]
pub enum Notification {
    /// New events for an `events.subscribe` subscription. Batched: at most a few per second.
    #[serde(rename = "events.batch")]
    EventsBatch(EventsBatch),
    /// Run state changes for a `runs.subscribe` subscription.
    #[serde(rename = "runs.changed")]
    RunsChanged(RunsChanged),
    /// Output for a `runs.subscribe_output` subscription.
    #[serde(rename = "runs.output")]
    RunsOutput(RunsOutput),
    /// Daemon-wide state changed. Sent to every connection; refetch what `topics` names.
    #[serde(rename = "state.changed")]
    StateChanged(StateChanged),
}

impl Notification {
    pub fn method(&self) -> &'static str {
        match self {
            Notification::EventsBatch(_) => "events.batch",
            Notification::RunsChanged(_) => "runs.changed",
            Notification::RunsOutput(_) => "runs.output",
            Notification::StateChanged(_) => "state.changed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct EventsBatch {
    pub subscription: SubscriptionId,
    /// A bounded number of the newest matching events, oldest first.
    pub events: Vec<Event>,
    /// Matching events left out of this live batch. The log has them all.
    #[ts(type = "number")]
    pub dropped: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RunsChanged {
    pub subscription: SubscriptionId,
    pub runs: Vec<RunSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RunsOutput {
    pub subscription: SubscriptionId,
    pub run: RunId,
    pub chunks: Vec<RunOutputChunk>,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StateChanged {
    pub status: SystemStatus,
    pub topics: Vec<StateTopic>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StateTopic {
    Pause,
    Config,
    Rules,
    Agents,
    Sources,
    Projects,
}

// ---- the table ----

macro_rules! rpc_methods {
    ($( $(#[$doc:meta])* $name:literal => $marker:ident, $handler:ident($params:ty) -> $result:ty; )*) => {
        /// Marker types for typed client calls, one per method.
        pub mod method {
            use super::*;
            $(
                $(#[$doc])*
                #[derive(Clone, Copy, Debug)]
                pub struct $marker;
                impl RpcMethod for $marker {
                    const NAME: &'static str = $name;
                    type Params = $params;
                    type Result = $result;
                }
            )*
        }

        /// Every method name, in table order.
        pub const METHOD_NAMES: &[&str] = &[$($name),*];

        /// A decoded request.
        #[derive(Clone, Debug)]
        pub enum Request {
            $( $marker($params), )*
        }

        impl Request {
            /// Decodes a method name and params member.
            pub fn parse(method: &str, params: Value) -> Result<Request, RpcError> {
                match method {
                    $( $name => Ok(Request::$marker(decode_params(params)?)), )*
                    other => Err(RpcError::method_not_found(other)),
                }
            }

            pub fn method(&self) -> &'static str {
                match self {
                    $( Request::$marker(_) => $name, )*
                }
            }
        }

        /// Implemented by the daemon: one method per RPC method. `Context` is per connection
        /// (its subscriptions, its notification sender).
        pub trait RpcHandler: Sync {
            type Context: Sync;
            $(
                $(#[$doc])*
                fn $handler(&self, cx: &Self::Context, params: $params)
                    -> impl Future<Output = Result<$result, RpcError>> + Send;
            )*
        }

        /// Routes a decoded request to its handler method and encodes the result.
        pub async fn dispatch<H: RpcHandler>(handler: &H, cx: &H::Context, request: Request) -> Result<Value, RpcError> {
            match request {
                $( Request::$marker(p) => encode_result(handler.$handler(cx, p).await?), )*
            }
        }

        /// `(method name, params TS type, result TS type)` for the TypeScript method map.
        pub(crate) fn ts_method_table(cfg: &ts_rs::Config) -> Vec<(&'static str, String, String)> {
            vec![ $( ($name, <$params as TS>::name(cfg), <$result as TS>::name(cfg)), )* ]
        }

        /// Visits every params and result type, for TypeScript export.
        pub(crate) fn visit_method_types(v: &mut impl ts_rs::TypeVisitor) {
            $( v.visit::<$params>(); v.visit::<$result>(); )*
        }
    };
}

rpc_methods! {
    /// Query the log with filters and cursor pagination.
    "events.query" => EventsQuery, events_query(EventQuery) -> EventPage;
    /// One event with its deliveries and runs.
    "events.get" => EventsGet, events_get(EventIdParams) -> EventDetail;
    /// Live events matching a filter, as `events.batch` notifications.
    "events.subscribe" => EventsSubscribe, events_subscribe(EventsSubscribeParams) -> Subscribed;
    "events.unsubscribe" => EventsUnsubscribe, events_unsubscribe(Unsubscribe) -> Empty;
    "events.replay" => EventsReplay, events_replay(ReplayParams) -> ReplayResult;
    /// External events; always typed `ext.<source>.…`.
    "events.ingest" => EventsIngest, events_ingest(IngestParams) -> IngestResult;
    /// Built-in events from trusted local reporters (`pulse run`).
    "events.report" => EventsReport, events_report(ReportParams) -> ReportResult;

    "rules.list" => RulesList, rules_list(RulesListParams) -> RuleList;
    "rules.get" => RulesGet, rules_get(NameParams) -> RuleInfo;
    "rules.upsert" => RulesUpsert, rules_upsert(RuleUpsertParams) -> RuleInfo;
    "rules.delete" => RulesDelete, rules_delete(NameParams) -> Empty;
    "rules.set_enabled" => RulesSetEnabled, rules_set_enabled(SetEnabledParams) -> RuleInfo;
    "rules.snooze" => RulesSnooze, rules_snooze(SnoozeParams) -> RuleInfo;
    /// Clears a snooze and an open circuit breaker.
    "rules.unsnooze" => RulesUnsnooze, rules_unsnooze(NameParams) -> RuleInfo;
    "rules.archive" => RulesArchive, rules_archive(NameParams) -> RuleInfo;
    "rules.reopen" => RulesReopen, rules_reopen(NameParams) -> RuleInfo;
    /// Dry run against a past event. No side effects.
    "rules.test" => RulesTest, rules_test(RuleTestParams) -> RuleTestResult;
    /// The rule's deliveries, newest first.
    "rules.history" => RulesHistory, rules_history(RuleHistoryParams) -> DeliveryPage;

    "agents.list" => AgentsList, agents_list(Empty) -> AgentList;
    "agents.get" => AgentsGet, agents_get(NameParams) -> AgentInfo;
    "agents.upsert" => AgentsUpsert, agents_upsert(AgentUpsertParams) -> AgentInfo;
    "agents.delete" => AgentsDelete, agents_delete(NameParams) -> Empty;
    /// The MCP URL and derived token for an agent.
    "agents.mcp_credentials" => AgentsMcpCredentials, agents_mcp_credentials(NameParams) -> McpCredentials;

    "sources.list" => SourcesList, sources_list(Empty) -> SourceList;
    "sources.upsert" => SourcesUpsert, sources_upsert(SourceUpsertParams) -> SourceInfo;
    "sources.delete" => SourcesDelete, sources_delete(NameParams) -> Empty;

    "runs.list" => RunsList, runs_list(RunsListParams) -> RunPage;
    "runs.get" => RunsGet, runs_get(RunIdParams) -> Run;
    "runs.output" => RunsOutputGet, runs_output(RunOutputParams) -> RunOutputPage;
    /// Run state changes, as `runs.changed` notifications.
    "runs.subscribe" => RunsSubscribe, runs_subscribe(RunsSubscribeParams) -> Subscribed;
    /// One run's output, as `runs.output` notifications.
    "runs.subscribe_output" => RunsSubscribeOutput, runs_subscribe_output(RunOutputSubscribeParams) -> Subscribed;
    /// Ends a `runs.subscribe` or `runs.subscribe_output` subscription.
    "runs.unsubscribe" => RunsUnsubscribe, runs_unsubscribe(Unsubscribe) -> Empty;
    "runs.cancel" => RunsCancel, runs_cancel(RunIdParams) -> RunSummary;
    "runs.approve" => RunsApprove, runs_approve(RunIdParams) -> RunSummary;
    "runs.reject" => RunsReject, runs_reject(RejectParams) -> RunSummary;
    /// A new run with the same snapshot and prompt.
    "runs.rerun" => RunsRerun, runs_rerun(RunIdParams) -> RunSummary;

    "projects.list" => ProjectsList, projects_list(Empty) -> ProjectList;
    "projects.trust" => ProjectsTrust, projects_trust(ProjectTrustParams) -> Project;
    "projects.untrust" => ProjectsUntrust, projects_untrust(ProjectDirParams) -> Project;

    "system.status" => SystemStatusGet, system_status(Empty) -> SystemStatus;
    "system.pause" => SystemPause, system_pause(Empty) -> SystemStatus;
    "system.resume" => SystemResume, system_resume(Empty) -> SystemStatus;
    "system.shutdown" => SystemShutdown, system_shutdown(Empty) -> Empty;
    "system.event_types" => SystemEventTypes, system_event_types(Empty) -> EventTypeList;
    "system.config_errors" => SystemConfigErrors, system_config_errors(Empty) -> ConfigErrorList;
}
