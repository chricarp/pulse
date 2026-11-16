# 0002: Architecture

Status: the v1 design. It is a starting point written before the code. When the code teaches us something, change this document in the same commit and say why at the bottom under "Changes".

## One paragraph

Sources watch the machine and emit normalized events. The engine persists each event to an append-only log and asks a **pure routing function** which rules it should fire. The answer (deliveries, runs, and the effects to perform) is committed in one SQLite transaction. A separate **effect worker** performs the effects: it spawns agents, streams their output into the run, and reports back. Clients (CLI, web UI, desktop shell, MCP) are views over services; they send intent and display state.

```
 sources ──NewEvent──▶ engine ──▶ event log (SQLite)
 (fs, git, schedule,      │
  calendar, ingest)       ├─▶ route(event, config, state, now)  ← pure, no I/O
                          │        │
                          │        ▼
                          │   deliveries + runs + outbox   (same transaction as the event)
                          │        │
                          │        ▼
                          │   effect worker ──▶ agent adapter ──▶ process ──▶ run output
                          │
                          └─▶ live fan-out: RPC subscribers; wakeups for MCP (which reads the log)
```

## Processes

| Process | What it is | Notes |
|---|---|---|
| `pulse start` | The daemon. Authoritative engine. | Runs headless. One per `PULSE_HOME`. |
| `pulse <cmd>` | CLI client. | Talks to the daemon over the RPC socket. |
| Web UI | Static app served by the daemon. | Works in any browser. |
| Desktop app | Electron shell. | Starts the daemon if needed, then loads the UI from it. |
| Agent runs | Children of the daemon. | Each in its own process group. |
| `pulse run -- cmd` | Wraps a command, reports its exit. | Works (and stays silent) when the daemon is down. |
| `pulse mcp --agent X` | stdio-to-HTTP proxy to the daemon's `/mcp`, with the agent's token. | For MCP clients that launch a server. Not a second MCP server. |

## Workspace

```
crates/
  pulse-contracts   Wire and config types. serde only. No I/O, no logic beyond validation.
  pulse-core        Pure logic: matching, permissions, dedupe, rate limits, debounce,
                    loop guard, prompt rendering, the routing function. Time is a parameter.
  pulse-store       SQLite: migrations, event log, deliveries, runs, run output, outbox, retention.
  pulse-sources     The Source trait and built-in sources: fs, git, schedule, calendar, process wrapper, ingest parsing.
  pulse-agents      The AgentAdapter trait, adapters (command, claude-code, codex), process supervisor.
  pulse-mcp         MCP server: tools, and the SEP-3415 adapter in one module behind a feature.
  pulse-daemon      Engine wiring, services, effect worker, config load/watch/write-back,
                    transports (RPC, ingest, static UI).
  pulse-cli         The `pulse` binary.
apps/
  web               React UI.
  desktop           Electron shell.
templates/          Demo rules shipped with the product.
docs/               internals/ (decisions), user/ (guides).
```

Dependency direction: `contracts ← core ← store, sources, agents, mcp ← daemon ← cli`. `core` never depends on tokio, the filesystem or the clock.

## Data directory

`$PULSE_HOME`, default `~/.pulse/`. Everything lives here:

```
pulse.toml      user config (source of truth for definitions)
pulse.db        SQLite (WAL): event log and runtime state
daemon.json     { pid, port, version, started_at } while the daemon runs
daemon.lock     flock held by the running daemon: one per Pulse home
token           daemon secret, mode 0600
runs/<id>/      per run: event.json (PULSE_EVENT_FILE) and the default working directory
logs/           rotated daemon logs
```

Development and tests **always** set `PULSE_HOME` to a temp dir.

## Events

Events use the [CloudEvents 1.0](https://cloudevents.io) JSON envelope with Pulse extension attributes:

```jsonc
{
  "specversion": "1.0",
  "id": "01JD…",                       // ULID, assigned by the engine; sortable, doubles as a cursor
  "type": "fs.file.created",
  "source": "pulse://fs/university",   // pulse://<kind>/<source name>
  "subject": "/home/me/University/Architettura/lecture-07.pdf",
  "time": "2026-11-16T09:41:07.123Z",
  "datacontenttype": "application/json",
  "data": { "path": "…", "dir": "…", "name": "lecture-07.pdf", "ext": "pdf", "size": 482113 },

  "pulsededupekey": "…",               // optional
  "pulsecoalesced": 14,                // raw signals folded into this event
  "pulsecausationrun": "01JD…",        // run believed to have caused this event, if any
  "pulsecausation": "exact|reported|inferred",  // how we know
  "pulsechaindepth": 1,                // 0 for events with no causing run
  "pulsereplayof": "01JD…"             // set on replays
}
```

### Type catalog (v1)

| Type | Subject | Notable `data` |
|---|---|---|
| `fs.file.created` / `.modified` / `.deleted` | path | `path, dir, name, ext, size` |
| `fs.file.moved` | new path | `from, path` |
| `fs.burst` | common directory | `dir, count, created, modified, deleted, sample[]` — a storm folded into one event |
| `git.commit.created` | repo path | `repo, branch, sha, message, author` |
| `git.branch.switched` | repo path | `repo, from, to` |
| `git.merge.conflicted` | repo path | `repo, files[]` |
| `git.push.completed` | repo path | `repo, remote, branch, sha` |
| `process.exited` | command line | `command[], cwd, exit_code, success, signal, duration_ms, output_tail` |
| `schedule.fired` | schedule name | `cron` or `every`, `scheduled_for` |
| `calendar.event.starting` / `.ended` | event title | `uid, title, start, end, location, minutes_until` |
| `ext.<source>.<anything>` | supplied | supplied. External sources cannot emit outside `ext.` |

The catalog is data (`pulse-contracts::catalog`), with a description, a JSON example and the list of path-carrying fields per type, plus a typed payload struct for each built-in type. `list_event_types`, the UI's rule editor, and the docs all read it. Every path in a built-in payload is absolute; `catalog::event_paths` returns the paths an event carries, which is what permission checks use.

## Pipeline

1. **Normalize** (in the source). Platform detail stops here.
2. **Coalesce** (in the source, as a pure state machine driven by a timer). Per-path quiet period so a file that is created and then written for three seconds becomes one `fs.file.created` when it settles. Storm folding: when pending changes under one scope exceed a threshold, they collapse into a single `fs.burst`. Memory is bounded: past the threshold we keep counts and a small sample, not paths.
3. **Backpressure.** Sources hand events to the engine through a bounded channel. A slow engine slows the source's sends; the source keeps coalescing in the meantime. Nothing buffers without limit.
4. **Dedupe** (engine). `dedupe key + TTL` in a bounded LRU.
5. **Persist.** Events are written in batches, one transaction per batch.
6. **Route** (pure). For each rule, in order: type and `where` match → rule state (enabled, global pause, snoozed, circuit open) → agent permission → loop guard → rule debounce → cooldown and rate limit → circuit breaker trip → agent backlog → render → supervised? The authoritative statement of this order, with the reasons for it, is the module doc of `pulse_core::routing`.
7. **Commit decisions.** Every rule that *matched* produces a **delivery** row saying what happened and why. "Why didn't my rule fire?" is always answerable from the log.
8. **Effects.** `Fire` writes a run (`queued`) and an outbox row. `AwaitApproval` writes a run (`pending_approval`) and no outbox row until someone approves.
9. **Fan-out to subscribers.** Batched: clients get at most a few notifications per second, each carrying a bounded number of events plus a count of what was dropped from the live view. The log is complete; the live feed is allowed to summarize.

### The routing function

```rust
// pulse-core. No I/O. `now`, pause, causation and randomness arrive in `ctx`; all state is `state`.
fn route(event: &Event, mode: RouteMode, routing: &Routing, state: &mut RouterState,
         ctx: &mut RouteContext) -> Vec<Decision>;
enum RouteMode<'a> { AllRules, Only(&'a [String]), DebounceElapsed(&'a str) }

// pulse-contracts (they cross the wire in `rules.test` and deliveries).
enum Outcome {
    Fire { prompt: Option<RenderedPrompt> },          // None: a command agent without a prompt
    AwaitApproval { prompt: Option<RenderedPrompt> },
    Debounced { until: Timestamp },
    Blocked { reason: BlockReason },
}
enum BlockReason {
    RuleDisabled, RuleSnoozed { until }, GlobalPause, PermissionDenied { needed: String },
    LoopSelfTrigger { run: RunId }, LoopChainDepth { depth: u32, max: u32 }, CircuitOpen { until },
    Cooldown { until }, RateLimited { until }, AgentBusy { backlog: u32, max: u32 },
    RenderFailed { message: String },
}
```

"Test this rule against a past event" calls the same function on a copy of the state (or a fresh one, to ignore history) and returns the decision and the rendered prompt, with no side effects. That is the payoff of keeping it pure. For a debounced rule, a second call with `DebounceElapsed` on the same copy shows what would fire.

A debounce is trailing: the rule fires once with the latest event when matching events stop for `debounce`. The daemon arms one timer for `RouterState::next_deadline()` and routes each `take_due()` entry with `DebounceElapsed`. If cooldown or the rate limit blocks an elapsed debounce, it is re-armed until the limit lifts instead of being dropped. `debounce_by = "subject"` keeps one timer per subject instead of per rule.

## Rules, agents, sources: the config file is the truth

Definitions live in TOML. The daemon loads `$PULSE_HOME/pulse.toml` plus any **trusted project files**, watches them, and reloads on change. The UI and CLI edit definitions through the daemon, which rewrites the owning file with a format-preserving editor (comments and ordering survive). There is no second copy of a rule in the database, so the two views cannot drift.

```toml
[[source]]
name = "university"
kind = "fs"                       # fs | git | schedule | calendar
path = "~/University"
ignore = ["**/.git/**"]           # added to built-in defaults

[[agent]]
name    = "study"
adapter = "claude-code"           # claude-code | codex | command | mcp
allow   = ["fs.*:~/University/**", "calendar.*"]
cwd     = "~/University"
timeout = "15m"
max_concurrent = 1

[[rule]]
name       = "new-lecture"
on         = "fs.file.created"    # a type glob, or a list of them
where      = { path = "~/University/**/*.pdf" }
debounce   = "10s"
cooldown   = "30s"
rate_limit = "10/hour"
supervised = false
enabled    = true
run        = { agent = "study", prompt = "Summarize {{subject}} and create flashcards next to it." }
```

- **`where`** is a small matcher language, not a programming language. Keys address `subject`, `source`, `type`, or a path into `data` (`path` means `data.path`). A string value is a glob, path-aware when it starts with `/` or `~`; numbers and booleans are equality; a list is "any of"; a table gives operators (`eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `in`, `contains`, `glob`, `exists`). A missing field never matches, except `{ exists = false }`. `source = "university"` matches the source name.
- **Globs**: `*` (never crosses `/` in path globs), `**` (a whole segment; `/a/**` also matches `/a`), `?`, `{a,b}`. In type globs `*` crosses `.`, so `fs.*` matches `fs.file.created`.
- **Permission scopes** (`allow`) are `"<type glob>"` or `"<type glob>:<path glob>"`. Default deny. A matching scope without a path covers the event. Otherwise the event must carry at least one path, and every path it carries must be inside a granted path glob. Paths are compared as text and must be clean absolute paths (no `.`, `..`, `//`); anything else is out of scope. Symlinks are not resolved. The router enforces this, and so do MCP queries and subscriptions.
- **Runtime state** that is not a definition lives in SQLite: snooze-until per rule, the global pause flag, circuit-breaker trips. Recent fire times (for cooldown, rate limit and breaker) are rebuilt from the delivery log at startup; pending debounces are not persisted.
- **Reverse states:** `enabled` ⇄ disabled (config), snooze ⇄ unsnooze (runtime; unsnooze also closes a tripped circuit breaker), `archived` ⇄ reopened (config; hidden from active lists, never routed, history kept), global pause ⇄ resume (runtime).
- **Project files.** A repo may check in a `pulse.toml`. It takes effect only after `pulse project trust <dir>`, and trust is bound to the file's content hash: when the file changes, it is inactive again until re-trusted. Cloning a repository must never be enough to run a command. Rule and agent names from a project file are prefixed `<project>/`. A project rule's `run.agent` names the project's own agent if it has one, otherwise the user's. Project files may not contain `[daemon]`, `[limits]` or `[retention]`, and their relative paths resolve against the project directory (scopes must still be absolute or `~/`).
- Config parsing (`pulse_contracts::config`) rejects unknown keys and reports file, line, item and field. The home directory is a parameter: `~` is expanded explicitly, never from the environment inside the library.
- A rule's identity is its name. History refers to rules by name, and each run stores a snapshot of what it executed.

## Runs

```
pending_approval ─approve─▶ queued ─▶ running ─▶ succeeded | failed | timed_out | cancelled
        ├─reject─▶ rejected      │                └─ daemon shut down or died ─▶ interrupted
        └─cancel─▶ cancelled     ├─cancel─▶ cancelled
                                 └─could not spawn─▶ failed
```

A running run is cancelled by signalling its process group; the supervisor's result is then recorded as `cancelled`. A graceful shutdown cancels every run the same way and records them `interrupted` ("the daemon shut down") once reaped; after a crash they become `interrupted` at the next start. Global pause also holds runs already `queued` (they start on resume); running runs continue. A rerun is an explicit request with the daemon token: it skips routing (no approval, cooldown or rate limit) and resends the stored prompt. A run without a configured `cwd` works in its own `runs/<id>/`. Chain depth: a run triggered by an uncaused event has depth 1; an event caused by a run carries that run's depth; a rule fires only if the new run's depth is at most `max_chain_depth`.

A run stores: the triggering event, the rule and agent names, the rendered prompt, the invocation (argv, cwd; not the environment), pid, timestamps, exit code, chain depth, a snapshot of the rule and agent config, and its output as ordered chunks. Output is capped per run: the first half of the cap is the head, then the most recent output; a `marker` chunk between them says how much was dropped.

`max_concurrent` limits running runs per agent and is enforced by the effect worker; up to `max_queued` more wait in `queued`. Routing blocks a fire as `agent_busy` only when that backlog is full. Supervised runs do not occupy the backlog until approved.

### Agent adapters

An adapter is two pure pieces and nothing else:

```rust
trait AgentAdapter {
    fn invocation(&self, agent: &AgentConfig, request: &RunRequest) -> Result<Invocation, AdapterError>; // argv, env, cwd, stdin
    fn parser(&self) -> Box<dyn OutputParser>; // native output lines → RunOutput { kind, text, data },
                                               // and write facts (see "Loop protection")
    fn reports_writes(&self) -> bool;          // whether that parser sees every write its tools make
}
```

One shared **supervisor** does the I/O for all of them: spawn in a new process group, stream output (`SupervisorEvent::Output { at, output }`, where `at` is when the line was read), enforce the timeout, and terminate the group (SIGTERM, grace period, SIGKILL) on cancel, timeout or shutdown, and after a natural exit too. Pulse only ever signals process groups it created and whose identity it can still verify (the leader is not reaped until the last signal). When the supervisor returns, everything still in the group has been sent SIGKILL. **Limit:** a descendant that leaves the group (`setsid`, `setpgid`: daemons, some multiplexers) escapes and is not killed; if it holds the output pipes, the run still ends and a `marker` says so. After a daemon crash, runs that were `running` become `interrupted`; we do not guess at PIDs. Details: [0006](0006-agent-adapters.md).

`command` never goes through a shell: template variables are substituted per argument, and the program itself cannot contain placeholders. Provider-specific detail (Claude Code's stream-json, Codex's JSONL) is preserved in `RunOutput.data` rather than flattened away. An agent with `adapter = "mcp"` is never run: it is an identity with scopes for an MCP client, and rules cannot target it.

The run environment contract (`PULSE_RUN_ID`, `PULSE_RULE`, `PULSE_AGENT`, `PULSE_CHAIN_DEPTH`, `PULSE_HOME`, `PULSE_EVENT_FILE`, `PULSE_EVENT_*`) is documented in `pulse_agents`.

## Loop protection

inotify and FSEvents do not say which process wrote a file. So we attribute what we know and guess only where we are blind, and every event says which it is (`pulsecausation`):

- **Exact:** every run gets `PULSE_RUN_ID` in its environment. `pulse run` and `pulse ingest` read it, so events they report carry exact causation.
- **Reported:** an adapter's parser sees the agent's tool calls, and reports two kinds of fact per run (`WriteFact`): "the agent wrote this path" (Claude Code's `Write`, `Edit`, `MultiEdit`, `NotebookEdit` carry it) and "the agent did something whose writes we cannot see" (a shell command, an MCP tool, a subagent, any tool the adapter does not know to be read-only, a hook, output the parser does not understand). A filesystem or git event on a path a run said it wrote, while it runs or within `causation_grace` (30 s) after, is attributed to it as `reported`, inside its working directory or outside it.
- **Inferred:** a filesystem or git event inside the working directory of a run whose writes are **opaque**, during the run or the grace period, is attributed to it and marked `inferred`. A run is opaque if its adapter cannot report writes at all (`command`, `codex`), from its first opaque tool call on, once it has reported more paths than it can remember (128 spellings: a file written through a symlinked folder takes two or three, so 40 to 128 files), and when it is reloaded after a daemon restart (nobody reads its output any more). Opaque is for good. A run that is not opaque (**observed**) claims nothing by inference: a file in its folder that it did not write is the user's, and so is a commit there. A run counts as active from just before its process is spawned, so a write in its first milliseconds is covered.
- **The decision** is pure (`pulse_core::causation`): the write state is part of `RecentRun`, passed in. The effect worker resolves reported paths (relative to the run's `cwd`; spelled as given, canonically, and canonically under the configured `cwd`, because Claude Code reports canonical paths and the fs source reports the configured spelling) and copies the run's state to the engine as facts arrive.
- **Ordering.** A write is reported when the tool call is printed, before the tool runs (measured with Claude Code 2.1.325: the line arrives 30 to 50 ms before the file's mtime), and the fs source holds every change back for its quiet period (2 s by default). The worker forwards facts without waiting for output appends, and the engine handles every pending control message before its next batch of events. The fact is late only if the agent's output reaches the daemon more than a quiet period after the write: the agent buffers its output, or the run's output channel stays full (more than 256 outputs behind a stalled store). Then the write is taken for the user's: it is not attributed, so if it matches the run's own rule, the rule fires once more, as a fresh chain (depth 1: chain depth does not bound it). That new run's own writes are normally reported in time and blocked. Only if the race is lost every time does it loop, and then cooldown, rate limit and the circuit breaker stop it. [0009](0009-end-to-end.md) lists what remains wrong in both directions.
- **Across restarts:** dedupe keys and recent runs are reloaded from the log at startup, so a keyed external event retried after a restart is still a duplicate, and a run orphaned by a crash still has its folder's events attributed to it (inferred) until its timeout.
- **Policy:** a rule never fires on an event caused by its own run (`LoopSelfTrigger`), however the cause is known. Other rules may, up to `max_chain_depth` (default 3).
- **Backstops that do not depend on attribution:** per-rule cooldown and rate limit, and a **circuit breaker** that snoozes a rule that fires too often in a short window (`CircuitOpen`).
- Blocked deliveries are recorded and shown in the UI. A loop that was stopped should be visible, not silent. A run that turns opaque says so in its output (a `marker`), so "why was my file taken for the agent's?" has an answer in the run.

## Protocol

JSON-RPC 2.0 over WebSocket at `ws://127.0.0.1:<port>/ws`. Auth: the daemon token. Method names are `noun.verb` (multi-word verbs in snake_case): `events.query`, `events.subscribe`, `events.replay`, `events.ingest`, `events.report`, `rules.list`, `rules.upsert`, `rules.test`, `runs.approve`, `runs.subscribe_output`, `system.pause`, and so on. The full set is one table in `pulse_contracts::rpc`, which generates the request enum, the `RpcHandler` trait the daemon implements (a missing method is a compile error), typed client markers, and the TypeScript `RpcMethods` map in `apps/web/src/contracts/index.ts`. Server-to-client notifications: `events.batch`, `runs.changed`, `runs.output` for subscriptions, and `state.changed` (with the topics to refetch) for every connection.

Every transport is a thin layer: decode, call **one service method**, map the error. Services: `events`, `rules`, `agents`, `runs`, `sources`, `projects`, `system`. The RPC server, the MCP tools, and the HTTP ingest endpoint all call the same methods.

The same HTTP listener also serves `POST /ingest` (external events, JSON or JSON lines), `/mcp` (MCP streamable HTTP), and `/` (the UI).

**Tokens.** The daemon token grants full control. Each agent gets a derived token (`HMAC(daemon secret, agent name)`) that is valid only on `/mcp` and only as that agent, so an agent's MCP configuration never contains a credential that can approve its own runs or edit rules. It is not a boundary against a process running as the user, which can read `$PULSE_HOME/token` (see [0003](0003-threat-model.md)). `/mcp` accepts no browser `Origin` at all.

**The browser** gets the daemon token without it appearing in a URL: `pulse open` puts a single-use, two-minute **login code** in the URL fragment, and the UI exchanges it at `POST /auth/login` for the token (see [0004](0004-daemon-and-config.md)).

**No client sends a credential to the port before whatever answers there has proven it knows it** (`GET /auth/proof`, see [0004](0004-daemon-and-config.md) and [0003](0003-threat-model.md)): the port is fixed, and while the daemon is stopped anyone can listen on it.

## MCP

Tools: `list_event_types`, `query_events`, `subscribe`, `unsubscribe`, `wait_for_events`. All of them act as the agent the bearer token names, resolved on every request, and are scoped by its `allow` list: the catalog lists only the types the agent's scopes can match, and history is filtered by scope *while it is read*, so pages and cursors never name an event the agent cannot see. MCP cannot approve runs or change rules.

**The log is the queue.** There are no per-subscription queues. A subscription is a saved filter plus a wakeup ("something you may see was committed"); `wait_for_events` reads the log after the cursor the caller passes back, so a slow agent is behind, never dropped. This needs events to become visible in id order, which the engine guarantees (one task assigns ids and commits) and the store enforces. A subscription ends when its agent is removed or loses access to every type.

`pulse mcp --agent X` is not a second server: it proxies stdio to `/mcp` with the agent's token.

The draft Events extension (SEP-3415) is implemented in **one module** behind the `sep3415` cargo feature (compiled into the shipped binary) and the runtime flag `[daemon] mcp_events_extension` (off by default). It offers `events/list`, poll and push (no webhook mode: a local daemon cannot meet its callback requirements), exists only on protocol revision 2026-07-28, and translates each CloudEvents envelope into the spec's `EventOccurrence`. When the draft changes, that module changes and nothing else does. Details: [0007](0007-mcp-and-sep-3415.md).

## Persistence

SQLite in WAL mode, one writer connection and one reader connection, each on its own thread behind a bounded channel (`pulse_store::StoreHandle`), so async code never blocks on SQLite. Migrations are numbered and append-only (`PRAGMA user_version`).

| Table | Purpose |
|---|---|
| `events` | The append-only log (full CloudEvents JSON plus indexed columns). Indexed by time, type, source, subject, causing run. A commit whose event ids do not increase past the newest stored one is refused. |
| `event_paths` | Every path an event carries, for indexed "events under this directory" queries. |
| `deliveries` | One row per (event, matching rule): outcome, reason, run id. |
| `runs` | Run state and metadata. |
| `run_output` | Ordered output chunks per run. |
| `outbox` | Effects to perform, with status and attempts. |
| `rule_state` | Snooze, circuit-breaker and last-fired per rule. |
| `kv` | Small runtime flags (global pause). |

**Retention:** events by age and by count, runs by age; an event referenced by a retained run is kept (and counts toward the event limit). Pruning runs in small batches off the hot path.

## Being a good citizen

- Idle means idle: no polling loops. Timers are armed for the next known deadline only.
- All queues are bounded. All maps keyed by external input are bounded.
- Shutdown: stop sources, stop accepting commands, terminate run process groups, flush, close the database, remove `daemon.json`.
- Logs rotate. The database prunes itself.

## Not in v1 (seams only)

Windows, clipboard/screen/app-focus/browser sources, email, cloud accounts, mobile, remote access, outgoing webhooks as a rule action. The transports already separate auth from routing, and sources and actions are behind traits, so each of these is an addition.

## Changes

_Record deviations from this document here: what changed, and why._

### 2026-11-16, Phase 1 (contracts, core, store, interfaces)

- **`route()` takes a mode.** `AllRules`, `Only(rules)` (replays limited to some rules, dry runs) and `DebounceElapsed(rule)`. A debounce has to come back through routing when its timer fires, and the checks after the debounce (cooldown, limits, backlog) must run at that point, not when the first event arrived.
- **Block reasons carry their data.** `RuleSnoozed { until }`, `CircuitOpen { until }`, `AgentBusy { backlog, max }` and a new `RenderFailed`. The UI needs "until when" and "how full" to explain a block. `Outcome` and `BlockReason` live in `pulse-contracts`, because `rules.test` and deliveries put them on the wire.
- **Circuit breaker split in two.** "Is it open?" is a rule-state check (step 2); "would this fire trip it?" comes after the rate limit (step 7). Checking an open breaker late would let a tripped rule occupy debounce slots.
- **Busy agents queue instead of dropping.** The design blocked a fire as `AgentBusy` when the agent was at `max_concurrent`, which loses work (a second PDF dropped while the first is summarized). Now the run is queued and the effect worker enforces concurrency; `agent_busy` only happens when `max_concurrent + max_queued` (new agent field, default 8) is full.
- **Elapsed debounces wait out cooldowns.** Otherwise a debounce that elapses inside a cooldown silently loses the event it was holding.
- **`debounce_by = "subject"`** (default `"rule"`). One timer per rule is right for "lint after edits"; one per subject is right for "summarize each new PDF".
- **Bounded debouncer degrades to no debounce.** At `limits.debounce_capacity`, new keys skip debouncing but still pass cooldown and rate limits. Dropping or evicting would lose events.
- **Chain depth defined precisely.** A run's depth is its event's depth + 1; a rule fires only if that is ≤ `max_chain_depth`. So the default 3 means at most three runs in a causal chain.
- **Scopes need a path to vouch for.** A path-qualified scope never covers an event that carries no paths, and every carried path must be covered. Paths must be clean and absolute; anything else is denied rather than normalized, because lexical `..` normalization is wrong with symlinks. `fs.file.moved` counts its `from` path.
- **`adapter = "mcp"` agents are identities, not runnables.** Rules cannot target them; they exist to get a scoped MCP token.
- **`unsnooze` also closes a tripped breaker.** A breaker trip is a snooze with a reason; one reverse action for both keeps the "every way in has a way out" rule simple.
- **Fire history is not persisted separately.** Cooldown, rate limit and breaker state are rebuilt from deliveries (`recent_fires`); `rule_state` keeps snooze, breaker and last fire. Pending debounces are lost on restart (known limit).
- **RPC additions.** `events.report` (built-in events from trusted local reporters; `pulse run` needs to emit `process.exited`, which `events.ingest` must never allow), `agents.mcp_credentials` (an MCP client needs the agent's derived token), `runs.subscribe` / `runs.unsubscribe` (run state changes need a subscription like output does), `rules.test` accepts an unsaved draft. `state.changed` carries the full status plus topics to refetch.
- **`event_paths` table.** "Events under `~/University`" needs an index; LIKE over JSON does not scale.
- **Store threading.** A writer thread and a reader thread behind bounded channels rather than `spawn_blocking`: writes are serialized without a lock, and a long query does not hold up the writer.
- **Config details.** `[daemon]`, `[limits]` and `[retention]` are settled (see `pulse_contracts::config`); project files cannot set them. Unknown keys are errors. `where` gained `type`, `glob` and `exists`.
- **TypeScript is one generated file**, `apps/web/src/contracts/index.ts`, with every wire type and the method map. `PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript` regenerates it; the same test fails in CI when it is stale.

### 2026-11-16, integration (daemon with MCP and the real supervisor)

The numbered docs 0004, 0006 and 0007 were written in parallel; this brings the parts of this page they could not edit in line with the code.

- **MCP pulls from the log.** `wait_for_events` reads by cursor; there are no per-subscription queues, and live subscriptions are only wakeups. Bounded queues that count overflow would drop events a cursor never loses.
- **History is scoped before paging.** Filtering after paging let a `text`, `subject` or `path` filter learn that a hidden event matched (a short page with a cursor). A scan budget (4096 hidden rows) keeps a page's work bounded; only past it does a cursor point at a row the agent cannot see.
- **Ids are ordered across restarts.** New ids continue after the newest stored one even when the clock reads earlier, and the store refuses out-of-order commits, because a cursor reader would skip such an event silently.
- **The MCP catalog is per agent**, and `/mcp` identity is decided by the MCP router's bearer layer only (one definition, `Services::mcp_agent`); the daemon's guard checks `Host` and refuses any `Origin` there.
- **SEP-3415** is poll and push only, needs protocol 2026-07-28, translates the envelope, and is compiled into `pulse` but off at runtime.
- **Data directory** has `daemon.lock` and `runs/<id>/` (per-run working directory and event file).
- **Runs:** graceful shutdown also records `interrupted`; pause holds queued runs; reruns skip routing.
- **Browser handover** is a single-use login code in the fragment, exchanged at `/auth/login`, not the token itself.
- **Supervisor:** output carries the time each line was read; the orphan guarantee and its `setsid` limit are stated here.

### 2026-11-16, reported writes (loop protection)

- **Attribution by inference had a false positive that broke the headline demo:** with `lecture-notes` (the agent's `cwd` is the watched folder) a PDF the user saved during a run, or 30 s after, was skipped as a self-trigger. Adapters now report the writes their agent's tool calls name, and a third causation kind, `reported`, sits between exact and inferred. Inference applies only to runs whose writes Pulse cannot see. It also closes a false negative: a reported write outside the run's `cwd` is attributed.
- **`AgentAdapter::reports_writes`** and **`OutputParser::take_facts`**, and **`SupervisorEvent::Writes`** (sent before the line's outputs). Classifying tools is a provider quirk and stays in the adapter; only `claude-code` reports writes. `codex` does not: its only recorded tool output is synthetic, and it reads files through shell commands, which would make almost every run opaque anyway.
- **A refused tool call counts** (Claude Code under `dontAsk` refusing `Bash` still makes the run opaque): the refusal arrives after the call, and over-attributing is the safe side. Not refined.
