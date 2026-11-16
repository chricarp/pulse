# 0004: The daemon, config and transports

Status: Phase 2, 2026-11-16. How `pulse-daemon` and `pulse-cli` implement [0002](0002-architecture.md), and where they differ from it. Each point says why.

## Engine

- **One task owns routing state.** The engine task holds `RouterState`, the dedupe table and the recent-runs list; nothing else touches them, so there are no locks around routing. It has two inputs: a control channel (config, snooze, pause, run started/ended, "recount backlogs", "copy the state for a dry run") and an event channel (sources, ingest, report, replay). Control is handled first (`biased`), so a run is known to attribution before its first file event is routed.
- **Batches.** After one event it drains up to 255 more that are already waiting, routes them all, and commits events, deliveries, runs, outbox rows and rule state in **one** `Store::commit`. Callers (ingest, report, replay) are answered after the commit, so an accepted id is always durable.
- **A failed commit leaves nothing**, and routing state is rebuilt from the log afterwards: `route()` had already counted fires that were never stored, and keeping them would hold the rule in a cooldown it never earned. Tested with an injected SQLite trigger.
- **One timer**, armed after every step for `RouterState::next_deadline()`.
- **Dedupe keys are per source** (`<source uri>\0<key>`): two sources may use the same key for different things.
- **Exact causation outlives the run.** `PULSE_RUN_ID` on a reported event is resolved from memory, else from the store, so `pulse run` inside a run that has just ended still attributes exactly. Inferred causation only uses runs that are active or within `causation_grace`.
- **Backlogs** (`queued + running` per agent) are recounted from the store whenever run states change outside routing (approve, cancel, end, rerun). Routing increments them itself for the fires it makes.

## Effect worker

- Claimed `start_run` rows wait in a FIFO per agent and start while the agent is under `max_concurrent` (read from the current config, else the run's snapshot). A row stays `claimed` until its run starts, so a crash returns it to pending. The run is re-read before it starts; one cancelled while it waited is skipped.
- **Global pause also holds queued runs.** 0002 says "nothing fires". A user who pauses because an agent misbehaves also expects runs already queued not to start; they start on resume. Running runs continue; cancel them explicitly.
- **Default working directory is per run**: `$PULSE_HOME/runs/<run id>/`, which also holds `event.json` (`PULSE_EVENT_FILE`). 0002 said `runs/`; a shared directory would let one run's files be attributed to another and would mix their outputs. Retention removes the directories of pruned runs.
- **Only the prompt text is stored** (`runs.prompt`), so a run started later gets a `RenderedPrompt` with the stored text and an empty `delimiter`. Adapters only send the text.
- **Graceful shutdown records `interrupted`** (with "the daemon shut down"): every run is cancelled through its token, the supervisor terminates its group, and the worker waits for each to be reaped. This needed `running → interrupted` in `RunTransition::Finish`; before, interrupted was only written at the next start, with the wrong end time. A crash is still recovered at the next start.
- A cancelled run is `cancelled`; one cancelled because of shutdown is `interrupted`.
- **Reruns skip routing**: they are an explicit request with the daemon token, so no approval, cooldown or rate limit applies. The run keeps its rule only while that rule still exists.

## Fan-out

- The engine publishes each committed batch once to a bounded `broadcast` (256 batches) and never waits. Each subscription is a task that filters into a buffer of the **200 newest** matching items and delivers at most **one batch per 250 ms** with a `dropped` count; the first batch after a quiet period goes out immediately. If the client's queue is full the buffer keeps summarizing; a client stuck for 30 s is disconnected. An idle subscription has no timer.
- A subscriber that falls behind the broadcast itself loses whole batches; those are counted in `dropped` by **batch**, not by matching event (which is unknown). `dropped > 0` means "the live view is incomplete; query the log", not an exact count.
- `runs.changed` keeps the **latest state per run** within a window: a run that went `queued → running → succeeded` in 250 ms is reported once, as `succeeded`. Views show state, not transitions; transitions are in the run itself.
- **Run output is pulled, not pushed.** An output subscription keeps a cursor and reads pages from the store when the run's `watch` counter moves (at most every 100 ms). A slow client reads bigger pages later; nothing is buffered for it and nothing is lost.
- `state.changed` is coalesced (100 ms) and carries the full status. It is also sent, with no topics, when only run counts changed (and the status really differs from the last one sent), so a client that shows "2 waiting for approval" (the desktop shell's badge) needs no run subscription. Before the web UI was wired to the daemon it was sent only for topic changes and the counts in it went stale.
- **Outcomes travel with events** to clients holding the daemon token: `events.batch` is `EventsBatchWithOutcomes` and `events.query` returns `EventPageWithOutcomes`, each with a compact `EventOutcome` (rule, delivery outcome, run) per delivery of its events. The engine publishes each commit's deliveries with its events, and deliveries recorded later for an earlier event (a debounce elapsing) with that event marked as not new, so subscribers send only the outcome. Agents over MCP get the base `EventsBatch`/`EventPage`: they are not told what other rules decided. Without this the UI asked `events.get` per visible row to show loop blocks, which does not survive a storm.
- **A subscribe call is answered before its first notification.** Each RPC subscription starts gated; the connection opens the gate once the response is queued (the queue is FIFO). Without it, output that was already stored could overtake the response.
- Live filters are evaluated by `pulse_core::filter::CompiledFilter`, which follows the store's query semantics so a subscription and a query agree.

## Config

- **Last good, per file.** If `pulse.toml` stops parsing, its last good version stays in effect and the errors (file, line, item, field) are in `system.config_errors` and `pulse status`. If the files parse but do not resolve together (a rule names a missing agent), the whole previous config stays. A project file has **no** last good: content that does not match its trusted hash is inert.
- **Write-back** (`rules.upsert`, `set_enabled`, `archive`, `agents.upsert`, `sources.delete`, …) edits the owning file with `toml_edit`, checks the result exactly as a reload would (parse, resolve with the other files, compile routing), writes a temp file and renames it, then reloads. Invalid definitions are refused with the errors and the file is not touched.
  - Old and new are compared **by meaning**, through the typed definition: `on = "x"` and `on = ["x"]` are equal, so writing back an unchanged rule changes no byte. Only keys whose meaning changed are rewritten; a rewritten value keeps its key's comments and its trailing comment. A key changed back to its default is removed unless a comment is attached to it, so `disable` then `enable` restores the file byte for byte.
  - New definitions are appended without keys that only restate defaults.
- **Trust** records (`name, dir, sha256, trusted_at`) live in the `kv` table under `trusted_projects` as JSON, so no migration was needed. Write-back to a project file re-binds the trust to the new hash: the user made the change with the daemon token. Any other change makes the project inert until `pulse project trust` again. Trusting requires the file to parse; names default to the directory name and must be valid names.
- **Watching**: the directories holding config files are watched (editors replace files by renaming), only for content changes to `pulse.toml` — not for access events: reading the file during a reload produces one, which in the first manual run caused a reload every ~150 ms. Changes settle for 150 ms before a reload. SIGHUP also reloads.
- Not hot-reloaded: `[daemon] port` (takes effect on restart; logged), `log_level`, and `debounce_capacity` (the debouncer is sized at start).

## Transports and auth

- One listener on `127.0.0.1`: `/ws`, `POST /ingest?source=<name>`, `POST /auth/login`, `/mcp`, and the UI. A guard runs first on every request: `Host` must be `127.0.0.1:<port>` or `localhost:<port>`; a present `Origin` must be this daemon's; `/ws` and `/ingest` need the daemon token; `/mcp` needs an agent token (the daemon token is refused there: MCP acts as an agent, and the identity is put into the request extensions for the MCP router).
- **Browsers** cannot set headers on a WebSocket, so the token may also come as the subprotocol `pulse.token.<token>`, offered together with `pulse` (which the server selects). Cookies were rejected: they are not port-scoped, so another server on 127.0.0.1 would receive them.
- **A browser cannot read the status of a refused upgrade**: a 401 and a daemon that is down both look like close code 1006, so the UI could not tell "log in again" from "reconnect". An upgrade that offers the `pulse` subprotocol with a missing or wrong token is therefore accepted and closed at once with **4401**, before anything is read. Other clients (Bearer) still get HTTP 401. The handshake, with its constants, is specified in `pulse_contracts::auth` and exported to TypeScript.
- **How the browser gets the token** (0002 suggested the token in the URL fragment): `pulse open` puts a **login code** in the fragment (`#login=<code>`), and the UI exchanges it at `POST /auth/login` for the token. The code is `expiry.HMAC(secret, expiry)`, valid for two minutes and once. The token itself would otherwise sit in `xdg-open`'s argument list, readable by other local users, and in browser history. The UI drops the fragment (`history.replaceState`) before anything else runs and keeps the token in `sessionStorage` (one tab; a reload keeps working, a new tab needs `pulse open`). The token is the daemon secret, so it survives daemon restarts; a 4401 makes the UI forget it and ask for `pulse open` instead of retrying.
- **Agent tokens** are `pa.<agent>.<hex HMAC-SHA256(secret, "pulse/agent-token/v1\0" + agent)>`: self-describing, so verification needs no lookup, and rotating the secret revokes all of them.
- The static UI is served without a token (it is the public app; it holds no data) but behind the Host check, with `X-Frame-Options: DENY` (it has approve buttons) and `nosniff`. Hashed `assets/` are cached for a year, the rest revalidates by ETag; paths without an extension fall back to `index.html`. Without `apps/web/dist` at build time a placeholder page says how to build the UI.
- **RPC**: one request per frame (batches get `invalid_request`), up to 16 in flight per connection (responses may arrive out of order), 64 subscriptions per connection, 4 MiB frames. `system.shutdown` replies before shutting down.
- Nothing logs headers, queries or bodies, so tokens and event content stay out of the log.

## Lifecycle

- One daemon per Pulse home, by `flock` on `daemon.lock` (released by the OS however the process ends). `daemon.json` (`pulse_contracts::status::DaemonInfo`) is written after the listener is bound and removed on exit. `pulse stop` asks for shutdown, then waits on the lock, so it returns when the daemon has really exited (runs terminated, database checkpointed).
- `pulse start --detach` re-executes itself in a new process group with `--ready-line` and returns once the child prints `ready <port>`; before that, the child's stderr is shown if it fails.
- Logs: `$PULSE_HOME/logs/pulse.log`, rotated at 10 MB, five old files kept. Retention runs at start and hourly, in batches of 500 with yields between.
- **Time is injected** (`Clock::System`, or `Clock::Tokio` which follows tokio's clock), so tests under `tokio::time::pause()` see a 10-minute debounce pass instantly.

## CLI

- `pulse ingest` decodes each stdin line as an `IngestEvent` itself and sends batches as soon as stdin has nothing more ready, so `tail -f log | pulse ingest -s ci` streams. `POST /ingest` uses `pulse_sources::ingest::parse` (objects, arrays, JSON lines). Both fill `causation_run` from `PULSE_RUN_ID`.
- `pulse run` reports through `events.report` with a 3 s bound and is silent when the daemon is unreachable; it exits with the command's code (128 + signal when killed, 127 when it cannot run the command).
- Exit codes: 0, 1 error, 2 usage, 3 the daemon is not running.

## Known limits

- The real supervisor, sources, ingest parser, process wrapper and MCP router are other packages; until they are merged, runs fail with "the supervisor is not implemented yet", `pulse run` and `POST /ingest` answer "not implemented", and `/mcp` answers 501.
- Pending debounces are not persisted (0002, unchanged).
- Live `dropped` counts are a summary, not an exact count, when a subscriber lags the broadcast (see Fan-out).
