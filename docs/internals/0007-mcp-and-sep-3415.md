# 0007: The MCP server and SEP-3415

Status: v1, 2026-11-16. Implements SEP-3415 as of PR modelcontextprotocol/modelcontextprotocol#3415 at commit `4165cc2e` (2026-11-14), on MCP protocol revision 2026-07-28, with `rmcp` 3.10.0. Code: `crates/pulse-mcp`.

Pulse launches agents, but it also tells agents that are **already running** what happened: they list event types, search history and subscribe, over MCP. This page says how, and why it is built this way. Section 4 is for whoever updates the SEP-3415 module next.

## 1. What exists

**Tools** (always on, every client):

| Tool | Does |
|---|---|
| `list_event_types` | The catalog entries the calling agent could ever receive, with description, subject, path fields, example. |
| `query_events` | History with filters (type globs, sources, path, subject glob, text, since/until), cursor pagination, newest or oldest first. |
| `subscribe` / `unsubscribe` | A saved filter owned by the agent. Returns a subscription id and a cursor at "now". |
| `wait_for_events` | Long poll: returns as soon as an event after the given cursor exists, or empty at the timeout. |

**Transports.** `pulse_mcp::router` is streamable HTTP for the daemon's `/mcp`. `pulse_mcp::stdio::proxy` is `pulse mcp --agent <name>`. `pulse_mcp::serve_io` serves a fixed, already-authenticated identity over any byte stream (tests, embedding).

**SEP-3415** (cargo feature `sep3415` and `McpConfig::events_extension`, both off by default): `events/list`, poll (`events/poll`) and push (`events/stream`). See section 4.

Nothing in this crate can approve a run, edit a rule or read outside the caller's scope. There is no backend method for the first two; for the third, every call passes the authenticated identity to the `Backend`, which applies the agent's `allow` list.

## 2. Decisions

### One MCP implementation, in the daemon; stdio is a proxy

`pulse mcp --agent X` is launched by an MCP client and must reach the daemon. Two designs were possible:

- **Serve MCP in the stdio process over a remote `Backend`** (an RPC client). The RPC needs the daemon token, which grants full control, and would need new agent-scoped RPC methods. Two MCP servers would have to behave identically.
- **Proxy stdio to the daemon's `/mcp`** with the agent's token. Chosen. One server, one place that resolves identity, and the stdio process holds only the agent token.

The proxy is a small amount of protocol knowledge: it sets `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` from each message (rmcp rejects 2026-07-28 requests without them), carries `Mcp-Session-Id` for pre-2026 clients, turns `notifications/cancelled` into closing that request's connection (how streamable HTTP cancels), and answers a request with a JSON-RPC error when the daemon cannot be reached instead of exiting.

### Identity per request

The HTTP middleware resolves `Authorization: Bearer` through `Backend::resolve_agent` and answers `401` before rmcp sees the request. The identity is stored in the request's extensions under a private type, so nothing else in the process can forge it, and tools read it from there on every call. There is no session-level identity to switch, and tool arguments that name an agent are rejected as unknown fields. On protocol 2026-07-28 rmcp builds a fresh handler per request anyway, which is why all state (subscriptions, stream counts) lives in a shared `Arc`, never in the handler.

### The log is the queue

The design called for per-subscription bounded queues that count overflow. Built instead: a subscription is a saved filter plus a *wakeup*. Its live backend subscription only says "something matching arrived"; events are always read from the log, after a cursor the caller passes back (the id of the last event it received). One function, `read_after`, backs `wait_for_events`, SEP poll and SEP push.

Why this is better:

- Nothing is buffered per subscription, so nothing can overflow and nothing is dropped. A slow agent is behind in the log and catches up page by page (`has_more`).
- Delivery is at-least-once with no acknowledgement bookkeeping: the caller's cursor is its acknowledgement. A result lost in transit (Codex's 60 s tool timeout firing while the server answers) is re-read with the same cursor.
- A subscription resumes without server memory. After it expires, or the daemon restarts, `subscribe` again and keep the last cursor: everything since it is delivered, within retention.
- Scoping is exactly the backend's: every event an agent receives came from `Backend::query_events` as that agent. Revocation is noticed on the next read.

Costs: a log query per wakeup per waiting subscription. Wakeups follow the daemon's live fan-out, which is already batched, and idle subscriptions cost nothing. Subscriptions are bounded (16 per agent, 256 in total), subscribing twice with the same filter returns the same subscription, and an entry nobody waits on for 30 minutes is removed by its own timer.

**Requirement on the daemon:** events must become visible to `query_events` in event-id order. A reader past id N never looks below N again, so an event that became visible after a higher id would be skipped. The single engine task assigning ids and committing in order satisfies this.

**"From now"** is the head of the log as the agent sees it (one newest-first query, using the row the backend examined even if the agent cannot see it), not the clock. With the clock, an event logged in the same millisecond just before a subscribe would count as new. The clock is used only for an empty log and for SEP `maxAgeMs`.

### Waiting with today's clients

Neither Claude Code nor Codex CLI consumes SEP-3415 events from arbitrary servers (section 3), so `wait_for_events` is how a running agent gets woken. Defaults: 45 s wait (Codex's `tool_timeout_sec` default is 60), up to 600 s on request, a progress notification at the start and every 15 s when the request carries a progress token (Claude Code resets its idle timeout on progress, and on HTTP the first message is what makes rmcp send response headers). A Claude Code call still running after two minutes moves to the background and its result arrives later as a task notification, so a long wait behaves like "subscribe, then get woken".

**Not built: answering `wait_for_events` as an MCP Task** for clients with the Tasks extension. It would need task state shared across per-request handlers, plus `tasks/get`, `tasks/result` and `tasks/cancel`, for a benefit only Claude Code has, and a plain long wait already backgrounds there. It can be added on top of the same `read_after` without changing the tools.

### Tool descriptions and results are prompts

Each description says what the tool is for, when to use it, what the arguments mean, and gives an example. Results that carry events are one text block: a line of JSON Pulse wrote (counts, cursors, flags), a fixed notice that what follows is untrusted data and that receiving an event is not permission to act, then the events as a JSON array between two lines `PULSE-UNTRUSTED-DATA-<128 random bits>`, fresh per result. Events are bounded by `pulse_core::prompt::bounded_event_json` (1 KiB per string by default, marked `…[N more bytes]`), and a result stops at 24 KiB, with the cursor pointing where it stopped. There is no `structuredContent`: clients may hand it to the model without the notice. As in the threat model, the fence lowers the odds of injection; it does not make a model immune.

### The catalog is scoped

`Backend::event_types` now takes the agent and returns only types its scopes can match. A model shown types it can never receive wastes context and subscribes to nothing.

### Not a safety boundary

The agent token keeps credentials that can approve or edit out of an agent's MCP configuration. It does not stop an agent process running as the user from reading `$PULSE_HOME/token` itself. Scopes control what Pulse tells an agent, not what the agent can do.

## 3. Findings that should outlast this version

Sources: the research notes for this work (labelled as read in a primary source [V], confirmed by experiment [E], or reported by a third party [R]) and this implementation's own tests and conformance runs [T].

- SEP-3415 has three delivery modes: poll, push, webhook. Webhook requires an authenticated principal, `https` callbacks, rejecting non-globally-routable callback addresses, endpoint verification and signing; a local daemon cannot honestly offer it. [V, SEP §Delivery Modes, §Webhook security]
- It exists only on protocol revision 2026-07-28 (`server/discover`, per-request `_meta` capabilities, no `initialize`), advertised as `capabilities.extensions["io.modelcontextprotocol/events"]`, not a top-level `events` key. [V, SEP §Capability Negotiation]
- The envelope is the SEP's `EventOccurrence` (`eventId, name, timestamp, data, cursor?, _meta?`), not CloudEvents. [V]
- Voted Accepted on 2026-11-11, PR still open and changing, error numbers provisional, no official SDK implements it. [V]
- Clients: Claude Code has no support (anthropics/claude-code#101434 open). Codex CLI 0.169.0 has an events client hard-wired to its hosted `codex_apps` server. Pi with `pi-mcp-adapter` and `@realmikekelly/pi-mcp-events` consumes push from any server. [V]
- rmcp 3.10.0: custom methods and notifications work (`on_custom_request`, `CustomNotification`), the service mounts in axum, and it builds with no TLS or system libraries. [E] It does not check `Mcp-Name` on `events/*` [V, E], its default SSE keepalive is the comment form the SEP rules out (turned off here) [V], it cannot negotiate `eventsListChanged` on `subscriptions/listen` [V], and it builds a fresh handler per 2026-07-28 request [V]. It maps a first-message `HeaderMismatch` to HTTP 400 [V, T] and `MethodNotFound` to 404 [V].
- rmcp's client moves a custom notification's `_meta` into `NotificationContext::extensions`, not `NotificationContext::meta`, so a client reading `meta` sees no subscription id. The wire is correct. [T]
- rmcp's client parses the push stream's final result as the look-alike `SubscriptionsListenResult`. [T]

## 4. SEP-3415 in Pulse

Everything is in `crates/pulse-mcp/src/sep3415.rs`; nothing outside it knows the spec's types or names.

| SEP | Pulse |
|---|---|
| event `name` | a Pulse event type, `fs.file.created`. `events/list` lists the built-in types the agent can see. `ext.*` is not listed (a namespace, not a name), but a concrete `ext.<source>.<name>` can be polled or streamed by an agent whose catalog includes `ext.*`. |
| `arguments` | Pulse filters: `path`, `subject`, `text`, `sources`. Unknown arguments are `InvalidParams`. |
| cursor | the id (ULID) of the last event delivered, or the head of the log for `null`. Opaque to clients; every Pulse type supports replay, so a cursor is never `null`. |
| `EventOccurrence` | `eventId` = `id`, `name` = `type`, `timestamp` = `time`, `data` = `data` (bounded; a non-object is wrapped as `{"value": …}`), `_meta` = `pulse/source`, `pulse/subject`, `pulse/causation`, `pulse/replayOf`, `pulse/shortened`. The table is tested in the module. |
| `payloadSchema` | the top-level fields of the catalog example and their JSON types, all optional. Loose on purpose, so later additions stay additive. |
| `maxAgeMs` | a floor computed from the clock; `truncated: true` when it is past the cursor. |
| heartbeat | `notifications/events/heartbeat` with the cursor after 30 s without a message (client may ask for 5 s to 60 s; the same on stdio). |
| cancellation | the request's cancellation token: `notifications/cancelled` on stdio, the connection closing on HTTP. Releases the backend subscription at once. |
| `Mcp-Name` | required and checked on HTTP for `events/poll`, `events/stream`, `events/subscribe`, `events/unsubscribe`; `HeaderMismatch` (HTTP 400) when missing or different. |
| errors | `NotFound` -32023 (`data.kind = "event"`), `Forbidden` -32024 (type outside the agent's catalog, or access revoked), `ResourceExhausted` -32025 (`data.limit = "streams"`, 16 per agent), `Unsupported` -32026 (webhook). Constants at the top of the module. |
| termination | the backend closing the live subscription, or a read returning `Unauthorized`, sends `notifications/events/terminated` with `Forbidden`. Other backend errors send `notifications/events/error` and the stream stays open. |

**Readings chosen where the SEP is unsettled or silent**, each picked to be easy to change: error codes are named constants; `Mcp-Name` strictness is one constant (`REQUIRE_MCP_NAME`) in case a real client turns out not to send it; `events/list` is not paginated; `nextPollMs` is a constant 5 s; client capability declarations are not required (the SEP makes them a MAY).

**Left out:** webhook delivery (`events/subscribe` answers `Unsupported` with `{"feature": "deliveryMode", "value": "webhook"}`); `listChanged` and `notifications/events/list_changed` (rmcp cannot carry `eventsListChanged`, and SEP-3371 disputes extensions adding it); the sketch-era top-level `events` capability; multi-name subscriptions (SEP open question 3); provenance metadata; long-poll `waitMs` on `events/poll` (not in the SEP); detecting a cursor that fell out of retention (replay resumes at the oldest retained event without `truncated`).

**Tested against:** an in-process rmcp client over stdio framing and the HTTP router in-process (`tests/sep3415.rs`); the stdio proxy end to end over a loopback port (`tests/proxy.rs`); and the conformance suite at modelcontextprotocol/conformance#521, head `76fef32d` (2026-11-08), against `examples/sep3415_fixture.rs`. Results: `events-discovery` 13/14, `events-poll` 31/32, `events-push` 12/22 with events flowing and 14/22 on a quiet fixture. Every push check that can be graded without the suite's fixture control tools passed in one of the runs (heartbeats need a quiet stream; `ResourceExhausted` needs a cap below three streams, which in turn fails the concurrency-exemption check). The rest are reported "not testable": a `subscriptions/listen` acknowledgement, a type without poll or push, injected upstream errors, revocation and catalog changes mid-run. Pi was not tried: it needs model credentials.

**When the SEP changes:**

1. Read the new SEP text and diff it against `4165cc2e`. Update the header of `sep3415.rs` and of this page with the new commit.
2. Wire names, error codes and envelope fields are constants and `json!` literals in `sep3415.rs`; change them there, then the expectations in `tests/sep3415.rs` and the envelope table test.
3. If rmcp gained events support or `Mcp-Name` checking for extension methods, delete ours rather than keeping two.
4. Run the conformance suite again: `cargo run -p pulse-mcp --features sep3415 --example sep3415_fixture -- <port> 1` (and `… 0` for push heartbeats), then from a checkout of the suite `npx tsx src/index.ts server --url http://127.0.0.1:<port>/mcp --scenario events-push --timeout 120000`.
5. When the SEP is merged and a client Pulse users run consumes it, consider turning `mcp_events_extension` on by default.

## 5. Client configuration

The agent's token comes from `agents.mcp_credentials` (`pulse agents mcp-credentials <agent>` in the CLI). Commands checked against `claude mcp add --help` (Claude Code 2.1.325) and `codex mcp add --help` (codex-cli 0.169.0).

**Claude Code, HTTP:**

```sh
claude mcp add --transport http pulse http://127.0.0.1:7477/mcp \
  --header "Authorization: Bearer <agent token>"
```

The header is stored in Claude Code's config in plain text. Do not use `--scope project` with a literal token: `.mcp.json` is checked in. In `.mcp.json`, write `"headers": { "Authorization": "Bearer ${PULSE_MCP_TOKEN}" }` instead.

**Claude Code, stdio:**

```sh
claude mcp add pulse -- pulse mcp --agent <agent>
```

**Codex CLI, HTTP:**

```sh
export PULSE_MCP_TOKEN=<agent token>
codex mcp add pulse --url http://127.0.0.1:7477/mcp --bearer-token-env-var PULSE_MCP_TOKEN
```

or in `~/.codex/config.toml`:

```toml
[mcp_servers.pulse]
url = "http://127.0.0.1:7477/mcp"
bearer_token_env_var = "PULSE_MCP_TOKEN"
# wait_for_events defaults to 45 s, under Codex's 60 s tool timeout. Raise both together.
tool_timeout_sec = 60
```

**Codex CLI, stdio:**

```sh
codex mcp add pulse -- pulse mcp --agent <agent>
```
