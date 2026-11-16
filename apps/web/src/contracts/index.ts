// Generated from crates/pulse-contracts. Do not edit.
// Regenerate: PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript

export type AgentConfig = { name: string, 
/**
 * Permission scopes. Default deny: an agent with no scopes receives nothing.
 */
allow: Array<Scope>, 
/**
 * Working directory for runs (`~` allowed). Default: the Pulse home's `runs/` directory.
 */
cwd?: string, timeout: Duration, 
/**
 * Runs executing at once.
 */
max_concurrent: number, 
/**
 * Runs waiting for a slot. Past this, new fires are blocked as `agent_busy`.
 */
max_queued: number, 
/**
 * Extra environment for runs. Trusted (it is config). Keys starting with `PULSE_` are
 * reserved.
 */
env?: { [key in string]: string }, } & ({ "adapter": "claude-code" } & ClaudeCodeSettings | { "adapter": "codex" } & CodexSettings | { "adapter": "command" } & CommandSettings | { "adapter": "mcp" } & McpSettings);

export type AgentInfo = { config: AgentConfig, origin: ConfigOrigin, 
/**
 * See [`RuleInfo::revision`].
 */
revision: string, running: number, queued: number, };

export type AgentList = { agents: Array<AgentInfo>, };

export type AgentUpsertParams = { agent: AgentConfig, project?: string, expected: Expected, };

export type BlockReason = { "reason": "rule_disabled" } | { "reason": "rule_snoozed", until: Timestamp, } | { "reason": "global_pause" } | { "reason": "permission_denied", needed: string, } | { "reason": "loop_self_trigger", run: RunId, } | { "reason": "loop_chain_depth", depth: number, max: number, } | { "reason": "circuit_open", until: Timestamp, } | { "reason": "cooldown", until: Timestamp, } | { "reason": "rate_limited", until: Timestamp, } | { "reason": "agent_busy", backlog: number, max: number, } | { "reason": "render_failed", message: string, };

export type CalendarEventData = { uid: string, title: string, start: Timestamp, end: Timestamp, location?: string | null, 
/**
 * Negative once the event has started.
 */
minutes_until: number, };

export type CalendarSourceSettings = { 
/**
 * An `http(s)://` ICS feed.
 */
url?: string, 
/**
 * A local `.ics` file.
 */
path?: string, 
/**
 * How often to re-read the calendar. Default 15m.
 */
refresh?: Duration, 
/**
 * How long before an event `calendar.event.starting` fires. Default 10m.
 */
lead?: Duration, };

export type Causation = "exact" | "reported" | "inferred";

export type ChangeKind = { "kind": "added" } | { "kind": "removed" } | { "kind": "changed", fields: Array<string>, };

export type ClaudeCodeSettings = { 
/**
 * The `claude` executable. Default: found on `PATH`.
 */
program?: string, model?: string, 
/**
 * Passed as `--permission-mode`. Default `dontAsk`: anything not pre-approved is denied,
 * since nobody can answer a prompt in an unattended run. Pulse never widens it.
 */
permission_mode?: string, 
/**
 * Each passed as `--allowedTools=<rule>`, like `"Edit"` or `"Bash(git diff:*)"`.
 */
allowed_tools?: Array<string>, 
/**
 * Each passed as `--disallowedTools=<rule>`.
 */
disallowed_tools?: Array<string>, extra_args?: Array<string>, };

export type CodexSettings = { 
/**
 * The `codex` executable. Default: found on `PATH`.
 */
program?: string, model?: string, 
/**
 * Passed as `--sandbox`. Default `read-only`. Pulse never widens it.
 */
sandbox?: string, 
/**
 * Passed as `approval_policy` (`-c`). Default `never`: nothing waits for an approval nobody
 * can give; a command that needs one fails and the model is told.
 */
approval_policy?: string, extra_args?: Array<string>, };

export type CommandSettings = { 
/**
 * argv template. Never run through a shell; each element is rendered on its own, so event
 * content cannot add arguments. The program (first element) may not contain placeholders.
 */
command: Array<string>, 
/**
 * What the process gets on stdin.
 */
stdin: StdinMode, };

export type ConfigError = { file?: string | null, 
/**
 * 1-based.
 */
line?: number | null, 
/**
 * 1-based, in characters.
 */
column?: number | null, 
/**
 * `rule "new-lecture"`, `agent "study"`, `[limits]`.
 */
item?: string | null, 
/**
 * `debounce`, `run.agent`, `allow[1]`.
 */
field?: string | null, 
/**
 * What is wrong and what was expected.
 */
message: string, };

export type ConfigErrorList = { 
/**
 * Problems that keep a file (or the whole config) from loading.
 */
errors: Array<ConfigError>, 
/**
 * Definitions in effect that cannot work as written: a rule that can never fire, an agent
 * whose program or working directory does not exist.
 */
warnings: Array<ConfigError>, };

export type ConfigOrigin = { "kind": "user", path: string, } | { "kind": "project", name: string, dir: string, path: string, };

export type DaemonInfo = { pid: number, port: number, version: string, started_at: Timestamp, };

export type DaemonProof = { 
/**
 * Random per daemon start (32 hex digits). Not secret.
 */
instance: string, 
/**
 * Lowercase hex HMAC-SHA256; see [`crate::auth`].
 */
proof: string, };

export type DebounceBy = "rule" | "subject";

export type Decision = { rule: string, agent: string, outcome: Outcome, 
/**
 * The chain depth a run created by this decision gets.
 */
chain_depth: number, };

export type DefinitionChange = { kind: DefinitionKind, name: string, change: ChangeKind, };

export type DefinitionKind = "rule" | "agent" | "source";

export type DeleteParams = { name: string, 
/**
 * `revision` from an editor or detail view; `any` from the CLI. `absent` is invalid.
 */
expected: Expected, };

export type Delivery = { id: DeliveryId, event: EventId, rule: string, agent: string, at: Timestamp, outcome: DeliveryOutcome, run?: RunId | null, };

export type DeliveryId = string;

export type DeliveryKind = "fired" | "awaiting_approval" | "debounced" | "blocked";

export type DeliveryOutcome = { "kind": "fired" } | { "kind": "awaiting_approval" } | { "kind": "debounced", until: Timestamp, } | { "kind": "blocked", reason: BlockReason, };

export type DeliveryPage = { 
/**
 * Newest first.
 */
deliveries: Array<Delivery>, next_cursor?: DeliveryId | null, };

export type Duration = string;

export type Empty = Record<string, never>;

export type Event = { specversion: SpecVersion, id: EventId, type: EventType, 
/**
 * `pulse://<kind>/<source name>`.
 */
source: string, subject?: string | null, time: Timestamp, 
/**
 * Always `application/json` for events Pulse writes.
 */
datacontenttype?: string | null, data: unknown, pulsededupekey?: string | null, 
/**
 * How many raw signals were folded into this event.
 */
pulsecoalesced?: number | null, 
/**
 * The run believed to have caused this event.
 */
pulsecausationrun?: RunId | null, pulsecausation?: Causation | null, 
/**
 * 0 for events with no causing run; otherwise the causing run's chain depth.
 */
pulsechaindepth: number, pulsereplayof?: EventId | null, };

export type EventDetail = { event: Event, deliveries: Array<Delivery>, runs: Array<RunSummary>, };

export type EventFilter = { types: Array<TypeGlob>, 
/**
 * Source URIs (`pulse://fs/university`) or bare source names (`university`).
 */
sources: Array<string>, 
/**
 * A glob over `subject`. A leading `~` is the user's home directory.
 */
subject?: string, 
/**
 * Events carrying a path equal to or under this absolute directory (or equal to this file).
 * A leading `~` is the user's home directory.
 */
path?: string, 
/**
 * Case-insensitive substring of the subject or the data.
 */
text?: string, since?: Timestamp, until?: Timestamp, 
/**
 * Events attributed to this run.
 */
caused_by?: RunId, };

export type EventId = string;

export type EventIdParams = { id: EventId, };

export type EventOutcome = { event: EventId, rule: string, outcome: DeliveryOutcome, 
/**
 * The run it created (fired, awaiting approval).
 */
run?: RunId | null, };

export type EventPageWithOutcomes = { 
/**
 * Every delivery recorded so far for the page's events, in the order they were recorded.
 * An event without entries matched no rule.
 */
outcomes: Array<EventOutcome>, events: Array<Event>, 
/**
 * Pass as `cursor` to continue; `None` at the end.
 */
next_cursor?: EventId | null, };

export type EventQuery = { filter: EventFilter, 
/**
 * The `next_cursor` of the previous page.
 */
cursor?: EventId, limit?: number, order: Order, };

export type EventType = string;

export type EventTypeInfo = { 
/**
 * A concrete type, or `ext.*` for the external namespace.
 */
type: string, description: string, 
/**
 * What the `subject` attribute holds.
 */
subject: string, 
/**
 * The fields that carry filesystem paths (checked against permission scopes).
 */
path_fields: Array<string>, example: unknown, };

export type EventTypeList = { types: Array<EventTypeInfo>, };

export type EventsBatchWithOutcomes = { 
/**
 * The deliveries recorded with this batch's events, plus any recorded since the previous
 * batch for matching events delivered earlier (a debounce that elapsed fires on an event
 * that was sent when it arrived). Outcomes of dropped events are not sent; query the log.
 */
outcomes: Array<EventOutcome>, subscription: SubscriptionId, 
/**
 * A bounded number of the newest matching events, oldest first.
 */
events: Array<Event>, 
/**
 * Matching events left out of this live batch. The log has them all.
 */
dropped: number, };

export type EventsSubscribeParams = { filter: EventFilter, };

export type Expected = { "kind": "absent" } | { "kind": "revision", revision: string, } | { "kind": "any" };

export type FsBurstData = { dir: string, count: number, created: number, modified: number, deleted: number, 
/**
 * A small, bounded sample of affected paths.
 */
sample: Array<string>, };

export type FsFileData = { path: string, dir: string, name: string, ext?: string | null, 
/**
 * Absent for deletions.
 */
size?: number | null, };

export type FsMovedData = { from: string, path: string, dir: string, name: string, ext?: string | null, size?: number | null, };

export type FsSourceSettings = { 
/**
 * The directory to watch (`~` allowed; relative to the project in project files).
 */
path: string, 
/**
 * Globs relative to `path` (or absolute) added to the built-in ignores.
 */
ignore?: Array<string>, recursive: boolean, 
/**
 * Per-path quiet period before a change is emitted. Default 2s.
 */
quiet?: Duration, 
/**
 * Pending changes under one directory beyond which they fold into one `fs.burst`.
 * Default 200.
 */
burst_threshold?: number, };

export type GitBranchSwitchedData = { repo: string, from?: string | null, to: string, };

export type GitCommitData = { repo: string, 
/**
 * `None` on a detached HEAD.
 */
branch?: string | null, sha: string, message: string, author: string, };

export type GitMergeConflictedData = { repo: string, 
/**
 * Absolute paths of conflicted files.
 */
files: Array<string>, };

export type GitPushCompletedData = { repo: string, remote: string, branch: string, sha: string, };

export type GitSourceSettings = { 
/**
 * The repository's working tree.
 */
path: string, };

export type IngestEvent = { 
/**
 * `build.failed` or `ext.<source>.build.failed`.
 */
type: string, subject?: string, 
/**
 * Default: when the daemon received it.
 */
time?: Timestamp, data: unknown, dedupe_key?: string, 
/**
 * The sender's `PULSE_RUN_ID`, if it was started by a run.
 */
causation_run?: RunId, };

export type IngestParams = { 
/**
 * The external source name; events become `ext.<source>.<type>`.
 */
source: string, events: Array<IngestEvent>, };

export type IngestResult = { accepted: Array<EventId>, 
/**
 * Dropped because their `dedupe_key` was seen within the dedupe TTL.
 */
duplicates: number, };

export type InvocationSummary = { program: string, args: Array<string>, cwd: string, };

export type JsonRpcNotification = { jsonrpc: JsonRpcVersion, } & ({ "method": "events.batch", "params": EventsBatchWithOutcomes } | { "method": "runs.changed", "params": RunsChanged } | { "method": "runs.output", "params": RunsOutput } | { "method": "state.changed", "params": StateChanged });

export type JsonRpcRequest = { jsonrpc: JsonRpcVersion, id?: RequestId, method: string, params?: unknown, };

export type JsonRpcResponse = { jsonrpc: JsonRpcVersion, 
/**
 * `null` when the request could not be read far enough to know its id.
 */
id: RequestId | null, } & ({ "result": unknown } | { "error": RpcError });

export type JsonRpcVersion = "2.0";

export type McpCredentials = { agent: string, url: string, token: string, };

export type McpSettings = Record<symbol, never>;

export type NameParams = { name: string, };

export type Notification = { "method": "events.batch", "params": EventsBatchWithOutcomes } | { "method": "runs.changed", "params": RunsChanged } | { "method": "runs.output", "params": RunsOutput } | { "method": "state.changed", "params": StateChanged };

export type Order = "newest_first" | "oldest_first";

export type Outcome = { "kind": "fire", prompt?: RenderedPrompt | null, } | { "kind": "await_approval", prompt?: RenderedPrompt | null, } | { "kind": "debounced", until: Timestamp, } | { "kind": "blocked", reason: BlockReason, };

export type OutputKind = "stdout" | "stderr" | "assistant" | "thinking" | "tool_use" | "tool_result" | "system" | "result" | "error" | "marker";

export type OutputStats = { 
/**
 * Bytes of output produced, including dropped bytes.
 */
bytes: number, 
/**
 * Bytes dropped from the middle to respect the per-run cap.
 */
dropped_bytes: number, };

export type ProcessExitedData = { command: Array<string>, cwd: string, exit_code?: number | null, success: boolean, signal?: number | null, duration_ms: number, 
/**
 * A bounded tail of combined stdout and stderr.
 */
output_tail: string, };

export type Project = { 
/**
 * The prefix for its definitions (`<name>/<rule>`).
 */
name: string, dir: string, file: string, state: TrustState, 
/**
 * SHA-256 of the trusted content, hex.
 */
trusted_hash?: string | null, current_hash?: string | null, trusted_at?: Timestamp | null, };

export type ProjectDirParams = { dir: string, };

export type ProjectList = { projects: Array<Project>, };

export type ProjectReview = { 
/**
 * The prefix its definitions would get.
 */
name: string, dir: string, file: string, state: TrustState, 
/**
 * SHA-256 of `text`. `projects.trust` with this hash trusts exactly this content.
 */
hash: string, 
/**
 * The file's text.
 */
text: string, 
/**
 * What it defines, as written (bare names). Empty when it does not parse.
 */
rules: Array<RuleConfig>, agents: Array<AgentConfig>, sources: Array<SourceConfig>, 
/**
 * Why it cannot be trusted as it is (it must parse on its own).
 */
errors: Array<ConfigError>, 
/**
 * For a file that changed since it was trusted: what differs from the trusted version, by
 * definition. `None` when there is no trusted version or Pulse did not keep its text
 * (it keeps it only for files up to 64 KiB).
 */
changes?: Array<DefinitionChange> | null, };

export type ProjectReviewParams = { dir: string, 
/**
 * The prefix its definitions would get. Default: the trusted name, else the directory's.
 */
name?: string, };

export type ProjectTrustParams = { 
/**
 * The directory containing `pulse.toml`. Absolute, or starting with `~`.
 */
dir: string, 
/**
 * The prefix for its definitions. Default: the directory's name.
 */
name?: string, 
/**
 * The `hash` of the [`ProjectReview`] the user was shown. If the file no longer has it,
 * nothing is trusted and the call fails with a `stale` error.
 */
hash: string, };

export type ProofQuery = { 
/**
 * Chosen by the client, fresh for each attempt: 32 to 128 lowercase hex digits.
 */
nonce: string, 
/**
 * Prove knowledge of this agent's MCP token instead of the daemon token.
 */
agent?: string, 
/**
 * Prove knowledge of this daemon instance's UI resume code instead of the daemon token.
 */
resume: boolean, };

export type Rate = string;

export type RejectParams = { id: RunId, reason?: string, };

export type RenderedPrompt = { text: string, 
/**
 * The per-run fence line around the data block.
 */
delimiter: string, 
/**
 * Something was cut to respect a size limit.
 */
truncated: boolean, 
/**
 * Placeholders whose value was absent (rendered as empty).
 */
missing: Array<string>, };

export type ReplayParams = { id: EventId, 
/**
 * Only route to these rules. Default: all rules.
 */
rules?: Array<string>, };

export type ReplayResult = { event: Event, deliveries: Array<Delivery>, };

export type ReportParams = { event: ReportedEvent, 
/**
 * The reporter's `PULSE_RUN_ID`.
 */
causation_run?: RunId, };

export type ReportResult = { 
/**
 * `None` if the event was deduplicated.
 */
id?: EventId | null, };

export type ReportedEvent = { "type": "process.exited" } & ProcessExitedData;

export type RequestId = number | string;

export type RpcError = { code: number, message: string, data?: unknown, };

export type RuleConfig = { name: string, description?: string, 
/**
 * Event type glob(s).
 */
on: TypeGlobs, where?: { [key in string]: WhereCondition }, 
/**
 * Trailing debounce: wait until matching events stop for this long, then fire once with
 * the latest.
 */
debounce?: Duration, debounce_by?: DebounceBy, 
/**
 * Minimum time between fires.
 */
cooldown?: Duration, rate_limit?: Rate, 
/**
 * Each run waits for a human to approve it.
 */
supervised: boolean, enabled: boolean, 
/**
 * Hidden from active lists and never routed; history is kept. `rules.reopen` reverses it.
 */
archived: boolean, run: RunSpec, };

export type RuleHistoryParams = { name: string, 
/**
 * Only these outcomes. Default: all.
 */
outcomes: Array<DeliveryKind>, cursor?: DeliveryId, limit?: number, };

export type RuleInfo = { 
/**
 * `config.name` is the full name (`project/rule` for project rules).
 */
config: RuleConfig, origin: ConfigOrigin, 
/**
 * Identifies this definition's content as written in its file (a hash). Send it back in
 * [`Expected::Revision`] when saving an edit of it.
 */
revision: string, state: RuleRuntimeStatus, };

export type RuleList = { rules: Array<RuleInfo>, };

export type RuleRef = { "kind": "saved", name: string, } | { "kind": "draft", rule: RuleConfig, project?: string, };

export type RuleRuntimeStatus = { snoozed_until?: Timestamp | null, 
/**
 * Set while the circuit breaker holds the rule. `rules.unsnooze` clears it too.
 */
circuit_open_until?: Timestamp | null, last_fired_at?: Timestamp | null, };

export type RuleTestParams = { rule: RuleRef, event: EventId, 
/**
 * Evaluate as if the rule had no history: no snooze, cooldown, rate or debounce state.
 */
ignore_runtime_state: boolean, };

export type RuleTestResult = { 
/**
 * The event's type and `where` match the rule.
 */
matched: boolean, 
/**
 * What routing would decide; `None` when it does not match.
 */
decision?: Decision | null, 
/**
 * When `decision` is `debounced`: what would happen once the debounce elapses (including
 * the rendered prompt).
 */
after_debounce?: Decision | null, };

export type RuleUpsertParams = { 
/**
 * `rule.name` is the bare name, without a project prefix.
 */
rule: RuleConfig, project?: string, expected: Expected, };

export type RulesListParams = { include_archived: boolean, };

export type Run = { id: RunId, 
/**
 * `None` only for runs whose rule was deleted before a rerun.
 */
rule?: string | null, agent: string, event: EventId, status: RunStatus, created_at: Timestamp, started_at?: Timestamp | null, ended_at?: Timestamp | null, 
/**
 * The rendered prompt, exactly as sent.
 */
prompt?: string | null, 
/**
 * What the run executes, rendered when the run is created (so an approval shows it) from
 * the snapshot; the run starts only if rendering it again gives the same result and the
 * agent is still defined as in the snapshot. `None` if it could not be rendered (the run
 * then fails to start).
 */
invocation?: InvocationSummary | null, pid?: number | null, exit_code?: number | null, signal?: number | null, 
/**
 * Why the run failed to start, was rejected, or was interrupted.
 */
error?: string | null, 
/**
 * 1 for a run triggered by an uncaused event; one more than the causing run's otherwise.
 */
chain_depth: number, rerun_of?: RunId | null, output: OutputStats, 
/**
 * The rule and agent as configured when it was created. A run never starts with an agent
 * definition that differs from the current one: see `docs/internals/0002-architecture.md`.
 */
snapshot: RunSnapshot, };

export type RunCounts = { running: number, queued: number, pending_approval: number, };

export type RunFilter = { rule?: string, agent?: string, 
/**
 * Any of these. Empty: all.
 */
status: Array<RunStatus>, event?: EventId, };

export type RunId = string;

export type RunIdParams = { id: RunId, };

export type RunOutputChunk = { seq: number, at: Timestamp, kind: OutputKind, text: string, data?: unknown, };

export type RunOutputPage = { chunks: Array<RunOutputChunk>, 
/**
 * Pass as `after` to continue: the last chunk's `seq`, or the `after` that was passed when
 * nothing more is stored yet (`None` only for a run with no output at all). A page with no
 * chunks is the end, for now.
 */
next_after: number | null, stats: OutputStats, 
/**
 * The run is terminal, so no more output will come.
 */
done: boolean, };

export type RunOutputParams = { id: RunId, 
/**
 * Chunks with `seq` greater than this. Default: from the start.
 */
after?: number, limit?: number, };

export type RunOutputSubscribeParams = { id: RunId, after?: number, };

export type RunPage = { 
/**
 * Newest first.
 */
runs: Array<RunSummary>, next_cursor?: RunId | null, };

export type RunSnapshot = { rule?: RuleConfig | null, agent: AgentConfig, };

export type RunSpec = { agent: string, 
/**
 * Required except for `command` agents.
 */
prompt?: string, };

export type RunStatus = "pending_approval" | "queued" | "running" | "succeeded" | "failed" | "timed_out" | "cancelled" | "rejected" | "interrupted";

export type RunSummary = { id: RunId, rule?: string | null, agent: string, event: EventId, status: RunStatus, created_at: Timestamp, started_at?: Timestamp | null, ended_at?: Timestamp | null, exit_code?: number | null, chain_depth: number, };

export type RunsChanged = { subscription: SubscriptionId, runs: Array<RunSummary>, };

export type RunsListParams = { filter: RunFilter, cursor?: RunId, limit?: number, };

export type RunsOutput = { subscription: SubscriptionId, run: RunId, chunks: Array<RunOutputChunk>, done: boolean, };

export type RunsSubscribeParams = { filter: RunFilter, };

export type ScheduleFiredData = { cron?: string | null, every?: Duration | null, scheduled_for: Timestamp, 
/**
 * A catch-up for a time that passed while the machine slept or the daemon was down. At most
 * one per schedule, for the latest missed time.
 */
late: boolean, };

export type ScheduleSourceSettings = { 
/**
 * Five fields: minute hour day-of-month month day-of-week.
 */
cron?: string, every?: Duration, 
/**
 * IANA name for `cron`; default is the system zone.
 */
timezone?: string, };

export type Scope = string;

export type SetEnabledParams = { name: string, enabled: boolean, };

export type SnoozeParams = { name: string, until: Timestamp, };

export type SourceConfig = { name: string, enabled: boolean, } & ({ "kind": "fs" } & FsSourceSettings | { "kind": "git" } & GitSourceSettings | { "kind": "schedule" } & ScheduleSourceSettings | { "kind": "calendar" } & CalendarSourceSettings);

export type SourceCounts = { healthy: number, degraded: number, failed: number, other: number, };

export type SourceErrorInfo = { at: Timestamp, message: string, };

export type SourceHealth = { state: SourceState, 
/**
 * Human-readable detail for the current state.
 */
message?: string | null, 
/**
 * When `state` last changed.
 */
since: Timestamp, last_event_at?: Timestamp | null, last_error?: SourceErrorInfo | null, events_emitted: number, };

export type SourceInfo = { config: SourceConfig, origin: ConfigOrigin, 
/**
 * See [`RuleInfo::revision`].
 */
revision: string, health: SourceHealth, };

export type SourceList = { sources: Array<SourceInfo>, };

export type SourceState = "starting" | "healthy" | "degraded" | "failed" | "stopped" | "disabled";

export type SourceUpsertParams = { source: SourceConfig, project?: string, expected: Expected, };

export type SpecVersion = "1.0";

export type Stale = { 
/**
 * The current revision of the definition (for `projects.trust`, the file's current content
 * hash); `None` when it does not exist.
 */
current?: string | null, };

export type StateChanged = { status: SystemStatus, topics: Array<StateTopic>, };

export type StateTopic = "pause" | "config" | "rules" | "agents" | "sources" | "projects";

export type StdinMode = "prompt" | "event" | "none";

export type Subscribed = { subscription: SubscriptionId, };

export type SubscriptionId = string;

export type SystemStatus = { version: string, started_at: Timestamp, 
/**
 * Global pause: rules are evaluated and recorded but nothing fires.
 */
paused: boolean, pulse_home: string, 
/**
 * The user's home directory, which `~` stands for in config and in event filters. Event
 * paths are absolute; clients use this to show them as `~/…`.
 */
user_home: string, port: number, sources: SourceCounts, runs: RunCounts, rules: number, agents: number, 
/**
 * Problems in config files; see `system.config_errors`. While the user's file has errors,
 * its last good version stays in effect; a project file with errors is inactive.
 */
config_errors: number, 
/**
 * Definitions that load but cannot work as written (a rule that can never fire, an agent
 * whose program is not installed); see `system.config_errors`.
 */
config_warnings: number, };

export type Timestamp = string;

export type TrustState = "untrusted" | "trusted" | "changed" | "missing";

export type TypeGlob = string;

export type TypeGlobs = Array<TypeGlob>;

export type UiLoginRequest = { code: string, };

export type UiLoginResponse = { 
/**
 * The daemon token. Send it as `Authorization: Bearer <token>`, or for a browser WebSocket
 * as the subprotocols `["pulse", "pulse.token.<token>"]` (see [`crate::auth`]).
 */
token: string, };

export type Unsubscribe = { subscription: SubscriptionId, };

export type WhereCondition = WhereScalar | Array<WhereScalar> | WhereOps;

export type WhereOps = { 
/**
 * Literal equality (strings are not globs here).
 */
eq?: WhereScalar, ne?: WhereScalar, gt?: number, gte?: number, lt?: number, lte?: number, 
/**
 * Literal membership.
 */
in?: Array<WhereScalar>, 
/**
 * Substring for strings, element equality for arrays.
 */
contains?: WhereScalar, glob?: string, exists?: boolean, };

export type WhereScalar = boolean | number | number | string;

/** Every RPC method with its params and result types. */
export type RpcMethods = {
  "events.query": { params: EventQuery; result: EventPageWithOutcomes };
  "events.get": { params: EventIdParams; result: EventDetail };
  "events.subscribe": { params: EventsSubscribeParams; result: Subscribed };
  "events.unsubscribe": { params: Unsubscribe; result: Empty };
  "events.replay": { params: ReplayParams; result: ReplayResult };
  "events.ingest": { params: IngestParams; result: IngestResult };
  "events.report": { params: ReportParams; result: ReportResult };
  "rules.list": { params: RulesListParams; result: RuleList };
  "rules.get": { params: NameParams; result: RuleInfo };
  "rules.upsert": { params: RuleUpsertParams; result: RuleInfo };
  "rules.delete": { params: DeleteParams; result: Empty };
  "rules.set_enabled": { params: SetEnabledParams; result: RuleInfo };
  "rules.snooze": { params: SnoozeParams; result: RuleInfo };
  "rules.unsnooze": { params: NameParams; result: RuleInfo };
  "rules.archive": { params: NameParams; result: RuleInfo };
  "rules.reopen": { params: NameParams; result: RuleInfo };
  "rules.test": { params: RuleTestParams; result: RuleTestResult };
  "rules.history": { params: RuleHistoryParams; result: DeliveryPage };
  "agents.list": { params: Empty; result: AgentList };
  "agents.get": { params: NameParams; result: AgentInfo };
  "agents.upsert": { params: AgentUpsertParams; result: AgentInfo };
  "agents.delete": { params: DeleteParams; result: Empty };
  "agents.mcp_credentials": { params: NameParams; result: McpCredentials };
  "sources.list": { params: Empty; result: SourceList };
  "sources.upsert": { params: SourceUpsertParams; result: SourceInfo };
  "sources.delete": { params: DeleteParams; result: Empty };
  "sources.set_enabled": { params: SetEnabledParams; result: SourceInfo };
  "runs.list": { params: RunsListParams; result: RunPage };
  "runs.get": { params: RunIdParams; result: Run };
  "runs.output": { params: RunOutputParams; result: RunOutputPage };
  "runs.subscribe": { params: RunsSubscribeParams; result: Subscribed };
  "runs.subscribe_output": { params: RunOutputSubscribeParams; result: Subscribed };
  "runs.unsubscribe": { params: Unsubscribe; result: Empty };
  "runs.cancel": { params: RunIdParams; result: RunSummary };
  "runs.approve": { params: RunIdParams; result: RunSummary };
  "runs.reject": { params: RejectParams; result: RunSummary };
  "runs.rerun": { params: RunIdParams; result: RunSummary };
  "projects.list": { params: Empty; result: ProjectList };
  "projects.review": { params: ProjectReviewParams; result: ProjectReview };
  "projects.trust": { params: ProjectTrustParams; result: Project };
  "projects.untrust": { params: ProjectDirParams; result: Project };
  "system.status": { params: Empty; result: SystemStatus };
  "system.pause": { params: Empty; result: SystemStatus };
  "system.resume": { params: Empty; result: SystemStatus };
  "system.shutdown": { params: Empty; result: Empty };
  "system.event_types": { params: Empty; result: EventTypeList };
  "system.config_errors": { params: Empty; result: ConfigErrorList };
};

export type RpcMethodName = keyof RpcMethods;

export const RPC_METHOD_NAMES = [
  "events.query",
  "events.get",
  "events.subscribe",
  "events.unsubscribe",
  "events.replay",
  "events.ingest",
  "events.report",
  "rules.list",
  "rules.get",
  "rules.upsert",
  "rules.delete",
  "rules.set_enabled",
  "rules.snooze",
  "rules.unsnooze",
  "rules.archive",
  "rules.reopen",
  "rules.test",
  "rules.history",
  "agents.list",
  "agents.get",
  "agents.upsert",
  "agents.delete",
  "agents.mcp_credentials",
  "sources.list",
  "sources.upsert",
  "sources.delete",
  "sources.set_enabled",
  "runs.list",
  "runs.get",
  "runs.output",
  "runs.subscribe",
  "runs.subscribe_output",
  "runs.unsubscribe",
  "runs.cancel",
  "runs.approve",
  "runs.reject",
  "runs.rerun",
  "projects.list",
  "projects.review",
  "projects.trust",
  "projects.untrust",
  "system.status",
  "system.pause",
  "system.resume",
  "system.shutdown",
  "system.event_types",
  "system.config_errors",
] as const satisfies readonly RpcMethodName[];

/** How clients authenticate; see `pulse_contracts::auth`. */
export const LOGIN_FRAGMENT_PARAM = "login";
export const LOGIN_PATH = "/auth/login";
export const LOGIN_CODE_TTL_SECS = 120;
export const PROOF_PATH = "/auth/proof";
export const PROOF_NONCE_MIN_HEX = 32;
export const PROOF_NONCE_MAX_HEX = 128;
export const PROOF_KEY_LABEL = "pulse/daemon-proof/v1";
export const RESUME_KEY_LABEL = "pulse/ui-resume/v1";
export const RESUME_CODE_PREFIX = "r.";
export const RESUME_STORAGE_KEY = "pulse.resume";
export const WS_PATH = "/ws";
export const WS_PROTOCOL = "pulse";
export const WS_TOKEN_PROTOCOL_PREFIX = "pulse.token.";
export const WS_CLOSE_UNAUTHORIZED = 4401;
/** The `code` of a `stale` RPC error; its `data` is a `Stale`. */
export const ERROR_STALE = -32007;

export type NotificationMethod = Notification["method"];
export type NotificationParams<M extends NotificationMethod> = Extract<Notification, { method: M }>["params"];
