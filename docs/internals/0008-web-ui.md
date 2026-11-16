# 0008: Web UI

Status: v1, 2026-11-16. Built against a mock daemon while the real RPC server was being written; see "What the daemon must do" before wiring the two together.

The UI is a view of the daemon. It holds no state the daemon does not know about, except view state: filters (in the URL), selection (in the URL), the theme override and a few toggles (in memory or `localStorage`). Everything else is a cache of what the daemon said, refreshed by its notifications.

## Libraries

| Package | Why |
|---|---|
| `react`, `react-dom` | The decision in [0001](0001-stack-and-desktop-shell.md). |
| `@tanstack/react-virtual` | Headless virtualization with variable row heights and scroll-to-index. Every list that can grow uses it: feed, runs, rule history, run output. |
| `vite`, `@vitejs/plugin-react`, `typescript`, `vitest`, `oxlint` | Build, types, unit tests, lint. No native compilation (this matters on machines without a C compiler). |
| `@playwright/test` | Browser tests and measurements. Pinned to the version whose Chromium build is installed. |

Not used, on purpose: a router (the History API and a 100-line `parseRoute` cover seven views), a state library (one `createStore` with `useSyncExternalStore`, 40 lines), a CSS framework (one stylesheet of tokens and components), an icon set (30 hand-drawn 16px paths), a form library, a markdown renderer (run output is text; see "Untrusted content").

## Structure

```
apps/web/src/
  contracts/   generated from pulse-contracts; never edited
  rpc/         the client, the WebSocket transport, the token handover
  store/       createStore, the feed buffer
  app/         the daemon cache, the feed controller, the delivery cache, router, keys, theme, toasts
  lib/         pure logic: filters, rule and agent forms, syntax checks, explanations, formatting
  ui/          primitives (buttons, pills, fields, dialogs, middle truncation), icons
  shell/       sidebar, banners, command palette, shortcut help
  views/       feed, runs, rules, agents, sources, projects, system
  mock/        the mock daemon (only in `--mode mock` builds)
```

## Transport

One `RpcClient` (`rpc/client.ts`) over anything shaped like a socket: the browser WebSocket in production, the mock in development and tests.

- **Calls** are typed by the generated `RpcMethods` map: `client.call("runs.approve", { id })` returns `RunSummary`. Responses are matched by id, so out-of-order answers are fine. Every call has a timeout (15 s by default).
- **While disconnected**, calls wait in a bounded queue (256) for the connection, within their timeout. Calls already sent when the connection drops fail with `disconnected` and are **never resent**: the daemon may already have approved, deleted or replayed.
- **Reconnect** with backoff (250 ms doubling to 30 s, with jitter), and immediately when the window regains focus or the network comes back. After three failures the banner says the daemon looks stopped and how to start it.
- **Subscriptions** (`events.subscribe`, `runs.subscribe`, `runs.subscribe_output`) are remembered and re-issued after every reconnect. Their params are a function, so run output resumes after the last `seq` received. Hooks report `subscribed`, `lost` (the feed marks a gap) and `error`.
- A notification for a subscription id the client has not learned yet (the subscribe response is still in flight) is held (at most 64) and delivered when the id arrives.
- **`state.changed`** updates the cached `SystemStatus` and refetches the lists its `topics` name.

## Token handover

`pulse open` opens `http://127.0.0.1:<port>/#token=<token>`. The fragment never reaches a server or a log. Before anything else runs, `rpc/token.ts` reads it, removes it from the address bar with `history.replaceState`, and keeps it in `sessionStorage` so a reload of that tab keeps working (a new tab needs `pulse open` again). The token is never logged and never put in a URL.

Browsers cannot set headers on a WebSocket, so the token is offered as a second subprotocol: `new WebSocket(url, ["pulse.v1", "pulse.token.<token>"])`. The daemon checks it and selects `pulse.v1` (it must not echo the token). A rejected token closes the socket with code **4401**; the app then stops retrying and explains how to open it properly. Without a token the app does not connect at all and shows the same explanation.

In development the app is served by Vite and proxies `/ws` to `PULSE_DAEMON` (default `http://127.0.0.1:7477`), rewriting `Origin` to the daemon's so the daemon's origin check passes. No origin is ever baked into the bundle.

## State

- **`Daemon`** (`app/daemon.ts`): connection state, `SystemStatus`, rules (including archived), agents, sources, projects, config errors, the event type catalog, and a **run index**. Lists load on every (re)connect and reload by `state.changed` topic. The run index is fed by one `runs.subscribe` for all runs: every active run plus the 300 newest finished ones. Pending approvals, badges and the window title come from it, so they are right in every view.
- **Views** fetch what only they show (an event's detail, a run, a rule's history) and refetch when a relevant run changes.
- **Store updates are bounded.** The feed and run output apply incoming data to plain structures and publish to React at most once per animation frame. Nothing is published while the tab is hidden; the structures keep up regardless.

## The feed buffer

`store/feedBuffer.ts` is the live window: the newest 5,000 events, newest first, plus **gap rows**.

- `events.batch` carries "a bounded number of the newest matching events, oldest first" and a `dropped` count. The dropped events are older than the batch's events and newer than what came before, so a gap row goes directly below the batch, with the count. Consecutive drops merge into one gap.
- On subscribe the feed subscribes **first**, then fetches the newest page from the log. Every insert is a merge by event id (ULIDs from one daemon sort in log order), which removes the overlap. A history event that lands inside a gap is one of the events the gap counted, so the count goes down; a gap filled completely disappears.
- A dropped connection adds a gap with an unknown count. After reconnecting, the newest page closes it if the page reaches below the gap (nothing was missed); otherwise it stays and says events may be missing.
- Past 5,000 events the oldest rows are evicted (they are in the log). Below the window, "older" pages come from `events.query`, up to 20,000 rows; past that the view asks for a narrower filter.
- A gap row's **Show from the log** opens that time range as a filter with an upper bound, which shows it from the log with no live tail.
- The live tail **holds** while you scroll away from the newest events or press pause, counting what arrived; returning to the top (or Home, or the button) resumes.

## Delivery markers in the feed

Batches carry events, not deliveries, but the feed must show at a glance what each event caused, loop protection above all. `app/deliveries.ts` fetches `events.get` lazily for rows on screen: only events whose type some rule listens to (a delivery needs a matching rule), only after the row has stayed on screen for 250 ms (a storm scrolling past asks for nothing), four at a time, cached up to 3,000 entries, and invalidated when a run for that event changes. See "Contract gaps": a summary of outcomes on the event would make this unnecessary.

## Run output

`runs.subscribe_output` with `after` = the last `seq` received; stored chunks come first, then live ones. Chunks are appended in place and published once per frame. The view follows the tail while scrolled to the bottom; scrolling up stops following and offers to return (End).

Rows are not measured. Measuring rows as they scroll by cost a synchronous React render per row, and 40,000 rows of output stuttered badly (long tasks up to 241 ms). Row heights are computed from the text instead: a fixed 18 px line height, a monospace font whose cell width is measured once, and wrapping at any character (`word-break: break-all`), so a line of `n` characters in a column of `w` cells takes `ceil(n / w)` lines. Heights are cached per chunk. Rows containing characters that may not be one cell wide (CJK, emoji, symbols from a fallback font) are still measured. The CSS for `.ch` and `chunkLayout` in `views/runs/output.ts` must change together.

Structured kinds read differently: tool calls are a header box with the tool name and arguments, tool results hang under it (collapsed past eight lines, red when `is_error`), thinking is dim italic, the final result and errors are boxed, stderr has a red rule, and Pulse's own `marker` (dropped output) is a dashed divider. Thinking, tool calls and system lines can be hidden.

## Untrusted content

Event fields and run output come from files, commits, calendars and processes ([0003](0003-threat-model.md)).

- Everything from outside reaches the DOM as text through React. There is no `dangerouslySetInnerHTML` anywhere, and nothing from an event or output becomes a link target; links in the UI are built from ids the app routes to.
- `visible()` shows bidi overrides, zero-width and control characters as `‹U+202E›` instead of letting them reorder or hide text, so a file named `invoice` + U+202E + `fdp.exe` cannot pass for a PDF. The event detail flags a subject that contains them.
- Prompts are shown split: the rule's instruction, then the fenced untrusted data block in a separate, dashed box.
- Run output is not rendered as Markdown, even when an agent writes Markdown.

## Keyboard

One `keydown` listener and a registry (`app/keys.ts`). Views register their bindings while mounted; the help overlay (`?`) and the System view list whatever is registered, and buttons show their shortcut. Single keys are ignored while typing in a field; `mod+` combinations and Escape are not. While a modal dialog is open it owns the keyboard.

Global: `mod+k` palette, `?` help, `g f/u/r/a/s/p/y` views, `g i` next approval, `shift+p` pause or resume everything, `n` new rule, `shift+t` theme. Lists: `j`/`k`, Escape. Feed: `/` filter, space pause the tail, Home newest. Runs: `shift+a` approve, `shift+x` reject, `shift+c` cancel, `shift+r` rerun, End follow output. Rules: `e` edit, `shift+e` enable or disable, `mod+enter` test, `mod+s` save.

## Design

Calm and dense: 13 px type, 30 px rows, monospace for machine data, tabular numerals for times and counts. Colour means state and nothing else: green fired or healthy, amber needs you (approval, snoozed, degraded, paused), blue waiting or running, orange-red **loop protection**, red failed, grey blocked or off. Light and dark follow the system; System → Theme (or `shift+t`) overrides it per browser. No animation repaints continuously; the only transitions are 60 ms hover backgrounds, off under reduced motion. Long paths truncate in the middle and keep the file name; ids and paths have copy buttons.

## The mock daemon

`src/mock/` implements every method in `RpcMethods` (the `Handlers` type makes a missing one a compile error) over in-memory state, behind a socket that speaks JSON strings, so the app and the tests run the same client code path as with the real daemon. It routes events with a simplified version of `pulse_core::routing` (same order of checks, same outcomes and block reasons), runs scripted agents whose output looks like each adapter's, batches live events like the daemon will (at most 100 per batch every 250 ms, the rest counted as dropped), omits default fields from definitions the way serde does, and sends `state.changed` with topics.

It seeds three hours of history: fires, debounces, every block reason the UI explains, a formatter whose own write is blocked by loop protection, a rule with a tripped circuit breaker, a snoozed rule, a disabled and an archived rule, runs in every status, a pending approval, a degraded and a failed source, project files in each trust state, and config errors. Controls (also in the palette in mock builds): an event storm, a run with megabytes of output, a supervised fire, dropping the connection, stopping and starting the daemon.

It is only imported when the build mode is `mock` (`pnpm dev:mock`, `pnpm build:mock`). `import.meta.env.MODE` is a build-time constant, so the production bundle contains neither the mock nor its controls.

Where the mock is simpler than the daemon: no dedupe, no `debounce_capacity`, the circuit breaker is a fixed 20 fires in 10 minutes, permission paths are compared with a JavaScript glob matcher, and the debounce elapsing records a second delivery for the event (fired) after the first (debounced). Treat these as guesses about the daemon, not as its specification.

## What the daemon must do for this app

- Serve `apps/web/dist` at `/`, and `index.html` for any other GET path that is not a file (the app routes with the History API: `/runs/<id>`, `/rules/<name>`, `/feed?type=…`).
- Accept the WebSocket at **`/ws`**, with subprotocols `pulse.v1` and `pulse.token.<token>`; select `pulse.v1`; close with **4401** for a missing or wrong token.
- Answer a subscribe call before sending that subscription's first notification (the client tolerates the reverse, briefly).
- Send `state.changed` when any field of `SystemStatus` changes, including run counts and source health, with `topics` possibly empty; include `rules`/`agents`/`sources`/`projects`/`config` when those lists change (source health counts as `sources`).
- `events.query`: `cursor` is exclusive and in the direction of `order` (`newest_first` returns ids below the cursor).
- `runs.subscribe` with an empty filter delivers every run change; a run leaving the filter's statuses is still reported once.
- `runs.subscribe_output` sends stored chunks after `after` (in pages), then live chunks, and a notification with `done: true` when the run is terminal (also when it already was).
- Expand `~` in `EventFilter.path` and `subject` (the UI passes what the user typed).

## Contract gaps and awkwardness

1. **No delivery summary on live or queried events.** The feed needs each event's outcomes at a glance, so it calls `events.get` per visible row. Suggest `EventsBatch` and `EventPage` carry `outcomes: { [event_id]: DeliverySummary[] }` (rule, outcome kind, block reason tag) for events with deliveries.
2. **Defaults missing on the wire contradict the generated types.** Serde skips `where`, `debounce_by` (rule), `ignore` (fs source), `allowed_tools`, `extra_args`, `env` (agent) when they hold defaults, but the TypeScript types mark them required. The UI reads them defensively. Either always serialize them or mark them optional in the generated types.
3. **`RunOutputParams.after` and `RunOutputSubscribeParams.after`** are `Option<u64>` in Rust but `number` (required) in TypeScript, because `#[ts(type = "number")]` drops the optionality.
4. **`rules.test` cannot show what a debounced rule would send.** It returns `debounced` and no prompt. The UI tests the same rule again as a draft without its debounce and shows that as "when the debounce elapses". A second decision (`after_debounce`) in `RuleTestResult` would be exact.
5. **"What it was coalesced from"** is only a count (`pulsecoalesced`). Nothing says which raw signals were folded; fine if that is the intent, but the UI cannot show more than the count.
6. **No WebSocket auth handshake in the contract.** The subprotocol scheme above is this app's proposal; it needs to be agreed and written down next to the threat model.
7. **`SystemStatus` lacks the home directory.** Paths in events are absolute (`/home/ada/…`) while config uses `~`; the UI cannot shorten paths to `~/…` or expand `~` in a filter itself.
8. **No way to discover project files.** `projects.list` returns known projects; trusting needs a directory typed by the user. Fine for v1, worth noting.
9. **`state.changed` topics have no `runs`**, which is right (runs have a subscription), but the status's run counts change far more often than anything else in it. If the daemon only sends `state.changed` for topic changes, the counts in `SystemStatus` go stale; the UI derives its counts from the run index for this reason.

## Testing

- Unit (Vitest, `pnpm --dir apps/web test`): the RPC client (out-of-order responses, errors with data, timeouts, queued calls, no resend after a drop, backoff and reset, 4401, resubscribe with fresh params, early notifications, unsubscribe races, `state.changed`), the feed buffer (bounds, batching, gaps and their counts, merging history, disconnection gaps), filters (EventFilter mapping, URL round trip, query syntax), the rule and agent forms (round trips of every field and adapter, problems per field), formatting of untrusted text and prompts, and the mock's scenarios through the real client.
- Browser (Playwright, `pnpm --dir apps/web test:browser`): the production build with the mock compiled in. Storms, gap rows, holding the tail, a dropped connection, keyboard paths, the palette, the filter syntax, shortcut discovery, a draft dry run, creating a rule and taking it through every reverse state, untrusting and re-trusting a project, megabytes of output, structured output, and the measurements below. Tests wait for conditions, never for time.

## Measurements

Chromium 156 (headless shell) on the development machine (Linux arm64, 8 cores, shared with other builds), 1440×900, production build with the mock in the page. The mock's own work (routing, serializing frames) runs on the same main thread, so these numbers overstate what the UI costs with the real daemon.

| Case | Frames | Long tasks |
|---|---|---|
| Feed, storm of 10,000 events in 3 s | p50 16.7 ms, p95 16.8 ms, max 33.3 ms | none |
| Feed, storm of 30,000 events in 3 s | p50 16.7 ms, p95 16.8 ms, max 16.8 ms | none |
| Feed, scrolling 20,001 rows from the log (3,000 px per frame) | p50 16.7 ms, p95 33.4 ms, max 50 ms | none |
| Run output, 6 MB produced (4 MB kept, ~40,000 chunks): open until every chunk is shown | 340–450 ms | one, 50–100 ms (mostly the mock serializing) |
| Run output, scrolling all of it (4,000 px per frame), rows measured (first version) | p50 33 ms, p95 217 ms, max 250 ms | 214, max 241 ms |
| Run output, scrolling all of it, heights computed (prose rows still measured) | p50 16.7 ms, p95 16.8–33.3 ms, max 33.4 ms | none |

Run them with `pnpm --dir apps/web test:browser`; `e2e/perf.spec.ts` and the storm and output tests print these lines.

The DOM holds fewer than 80 feed rows and 200 output rows at any time.
