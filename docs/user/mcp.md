# Connecting an MCP client

Rules start agents. MCP is the other direction: an agent that is **already running** (a Claude
Code or Codex session, any MCP client) asks Pulse what happened. It can list the event types it
may see, search the log, and subscribe and wait for new events. MCP cannot approve runs, change
rules, or read outside the agent's scopes.

## 1. Give the client an identity

Every MCP client acts as one Pulse agent, and sees only what that agent's `allow` list covers.
For a client that Pulse never starts, use an `mcp` agent:

```toml
[[agent]]
name = "editor"
adapter = "mcp"
allow = ["git.*:~/code/**", "process.exited:~/code/**"]
```

An existing `claude-code`, `codex` or `command` agent works too.

## 2. Get its credentials

```sh
pulse agents mcp-credentials editor
```

prints the URL (`http://127.0.0.1:7477/mcp`), the agent's token, and configuration to paste.
The token is `pa.editor.<hex>`: valid only on `/mcp`, only as `editor`. It cannot be used for
the CLI, the UI, or approvals. It is derived from the daemon's secret, so it stays the same
across restarts.

## 3. Configure the client

**Claude Code, stdio** (no token in the client's config):

```sh
claude mcp add pulse -- pulse mcp --agent editor
```

**Claude Code, HTTP:**

```sh
claude mcp add --transport http pulse http://127.0.0.1:7477/mcp \
  --header "Authorization: Bearer <agent token>"
```

The header is stored in Claude Code's config in plain text. Do not use `--scope project` with a
literal token: `.mcp.json` is checked in. Write
`"headers": { "Authorization": "Bearer ${PULSE_MCP_TOKEN}" }` there instead.

**Codex CLI:**

```sh
codex mcp add pulse -- pulse mcp --agent editor
```

or, over HTTP, in `~/.codex/config.toml`:

```toml
[mcp_servers.pulse]
url = "http://127.0.0.1:7477/mcp"
bearer_token_env_var = "PULSE_MCP_TOKEN"
tool_timeout_sec = 60   # wait_for_events waits 45 s by default; keep this above it
```

These commands were written against Claude Code 2.1.325 and codex-cli 0.169.0 (`--help`).

### `pulse mcp --agent <name>`

A bridge for clients that launch a server over stdio. It is not a second server: it forwards to
the daemon's `/mcp` as that agent. It finds the port in `$PULSE_HOME/daemon.json` and the token
by deriving it from `$PULSE_HOME/token`, or from `PULSE_MCP_TOKEN` if set (which must be that
agent's). Stdout carries only MCP messages. It exits 3 if no daemon is running, 1 for an unknown
agent, 2 for a token that belongs to another agent. If the daemon stops while the client is
connected, each request gets an error and the bridge keeps running.

## The tools

| Tool | Does |
|---|---|
| `list_event_types` | The event types this agent could ever receive, with a description and an example. |
| `query_events` | Search history: `types`, `sources`, `path`, `subject` (glob), `text`, `since`, `until`, `cursor`, `limit` (1–50, default 20), `order`. |
| `subscribe` | Save a filter (`types`, `sources`, `path`, `subject`, `text`). Returns a `subscription` id and a cursor at "now". |
| `wait_for_events` | Wait (default 45 s, up to 600 s) until an event after `cursor` exists; returns it and the next cursor. |
| `unsubscribe` | Remove a subscription. |

The log is the queue: nothing is buffered per subscription, so a slow agent is behind, never
dropped. Pass back the last cursor to continue; after a restart, subscribe again and keep the old
cursor. Subscriptions are limited to 16 per agent and expire after 30 minutes without a wait.

## Limits

An agent's token can only take its own share of Pulse, so a client stuck in a loop cannot make
Pulse unreachable for the CLI, the UI or other agents. The limits are fixed, far above what one
or a dozen clients need, and per agent: clients sharing an agent share them.

| Limit | What the client sees past it |
|---|---|
| 32 requests in flight per agent, counting open streams and long waits (256 for all agents) | HTTP 429 (503 for the total) with a JSON-RPC error saying why and `Retry-After: 5`. Finish or close a request, then retry. |
| 4 `wait_for_events` at once per subscription, 16 per agent | A tool error. Wait for one to return; the cursor keeps your place, so nothing is lost. |
| 16 subscriptions per agent | A tool error listing yours. Subscribing with the same filter reuses one. |
| 48 sessions per agent, for clients on protocol versions before 2026-07-28 | Nothing: the agent's least recently used idle session is closed instead. If that was yours, your next request gets HTTP 404 and your client opens a new session. |
| 64 KiB per request | HTTP 413. |
| 24 KiB of events per result | Fewer events, with `has_more` (or `next_cursor`) set: call again to get the rest. |

Pulse closes the connection after each response (`Connection: close`); HTTP clients reconnect on
their own.

Event content in tool results is fenced as untrusted data, with a notice that receiving an event
is not permission to act. As with prompts, this lowers the odds of injection; it does not make a
model immune.

## The experimental events extension (SEP-3415)

Pulse implements the draft MCP Events extension (`events/list`, poll and push; no webhooks) for
clients that speak protocol revision 2026-07-28. It is **off by default**, tracks a draft that is
still changing, and will change with it. No mainstream client consumes it yet; the tools above
are what works today. To try it:

```toml
[daemon]
mcp_events_extension = true
```

then restart the daemon (`pulse stop`, `pulse start`). A poll result holds at most 24 KiB of
events (`hasMore` then says to poll again), and an agent can have 8 push streams open; the
ninth is refused with `ResourceExhausted`. Details:
[docs/internals/0007-mcp-and-sep-3415.md](../internals/0007-mcp-and-sep-3415.md).
