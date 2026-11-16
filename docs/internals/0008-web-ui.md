# 0008: Web UI

Status: v1, 2026-11-16. Built against a mock daemon while the real RPC server was being written, then wired to the real daemon and used through every view; "Wiring it to the real daemon" records what disagreed and how it was settled.

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
  rpc/         the client, the WebSocket transport, the login
  store/       createStore, the feed buffer
  app/         the daemon cache, the feed controller, event outcomes, home directory, router, keys, theme, toasts
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

## Login

The handshake is specified in `pulse_contracts::auth` (with its constants exported to `contracts/index.ts`), because the UI, the daemon and the desktop shell must agree on it.

- `pulse open` (or the desktop shell) opens `http://127.0.0.1:<port>/#login=<code>`: a code that works once, for two minutes, and only on the daemon that was running when it was minted. `pulse open` does it through a mode-0600 page in the Pulse home that redirects there, so the code is in no process's arguments. Before anything else runs, `rpc/login.ts` removes the fragment with `history.replaceState` and posts the code to `POST /auth/login`, which answers with the daemon token.
- **The token is kept in memory only.** Nothing in `sessionStorage` or `localStorage` holds it, because a program that takes the port while the daemon is stopped serves its own page on this origin when the tab reloads, and that page can read everything stored there.
- **A reload** signs in with a resume code instead (`ReloadKeeper`): each time a daemon instance proves itself, the page computes that instance's resume code from the token, keeps it in memory, and writes it to `sessionStorage` only on `pagehide`. The next load takes it out of storage before anything else, asks the daemon to prove it knows that code (`resume=true`), and only then exchanges it at `POST /auth/login`. The daemon accepts only its own instance's code, so a stolen one is worthless once the daemon that issued it has stopped, which it must have for anything else to hold its port. A reload works while the daemon runs (also after a restart, once the tab has reconnected); a reload while it is down or just after a restart needs `pulse open`. Only pages on `127.0.0.1` write one. Opening the same link again in a tab falls back to its resume code.
- **Before every WebSocket attempt** the socket asks `GET /auth/proof` (`rpc/proof.ts`, WebCrypto) and opens the WebSocket only if the answer proves the token. Otherwise it reports close code 4900 (local, never on the wire) and the client retries with backoff, as for a daemon that is down; the banner says that something on the port could not prove it is the daemon, and what to do.
- The WebSocket offers the subprotocols `["pulse", "pulse.token.<token>"]`; the daemon selects `pulse` and never echoes the token.
- A browser cannot read the status of a refused upgrade (a 401 looks like a daemon that is down: close 1006), so the daemon accepts a browser upgrade with a bad token and closes it with **4401**. The app then drops its resume code, stops retrying and shows how to log in again. Any other close means reconnect with backoff.
- Each failure has its own screen (`NoAccess`, `data-reason`): no code, a refused code (expired, spent, or from another home; it also says that if the link was not used before something else may have used it, and how to rotate the token), a resume code for a daemon that has restarted, a port that could not prove it is the daemon, a daemon that did not answer the exchange, and a token the daemon no longer accepts.

In development Vite proxies `/ws` and `/auth` to `PULSE_DAEMON`, rewriting `Origin` to the daemon's so its origin check passes; the daemon proves itself for its own port, so the dev build is told that port (`__PULSE_DEV_DAEMON_PORT__`). No origin is ever baked into the bundle. See `apps/web/README.md` for the loop.

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

The feed must show at a glance what each event caused, loop protection above all. The daemon sends it with the events: every `events.batch` and every `events.query` page carries an `EventOutcome` (rule, delivery outcome, run) per delivery of its events, and a batch also carries outcomes recorded later for events it sent earlier (a debounce that elapsed). `app/outcomes.ts` merges them by event; the detail pane, which fetches the full deliveries anyway, replaces an event's entry. No request is made per row. (The first version fetched `events.get` for rows that stayed on screen; under a storm that is one request per row, and it could not know about a fire that happens ten seconds later.)

## Run output

`runs.subscribe_output` with `after` = the last `seq` received; stored chunks come first, then live ones (the daemon reads new chunks from the store at most every 100 ms), then `done`. Chunks are appended in place and published once per frame. The view follows the tail while scrolled to the bottom; scrolling up stops following and offers to return (End).

Rows are not measured. Measuring rows as they scroll by cost a synchronous React render per row, and 40,000 rows of output stuttered badly (long tasks up to 241 ms). Row heights are computed from the text instead: a fixed 18 px line height, a monospace font whose cell width is measured once, and wrapping at any character (`word-break: break-all`), so a line of `n` characters in a column of `w` cells takes `ceil(n / w)` lines. Heights are cached per chunk. Rows containing characters that may not be one cell wide (CJK, emoji, symbols from a fallback font) are still measured. The CSS for `.ch` and `chunkLayout` in `views/runs/output.ts` must change together.

Structured kinds read differently: tool calls are a header box with the tool name and arguments, tool results hang under it (collapsed past eight lines, red when `is_error`), thinking is dim italic, the final result and errors are boxed, stderr has a red rule, and Pulse's own `marker` (dropped output) is a dashed divider. Thinking, tool calls and system lines can be hidden.

## Untrusted content

Event fields and run output come from files, commits, calendars and processes ([0003](0003-threat-model.md)).

- Everything from outside reaches the DOM as text through React. There is no `dangerouslySetInnerHTML` anywhere, and nothing from an event or output becomes a link target; links in the UI are built from ids the app routes to.
- `visible()` shows control and invisible formatting characters (bidi controls, zero-width and other default-ignorable characters, fillers, separators, tag characters; the list of [0003](0003-threat-model.md) §1) as `‹U+202E›` instead of letting them reorder or hide text, so a file named `invoice` + U+202E + `fdp.exe` cannot pass for a PDF. The event detail flags a subject that contains them.
- **Views do not call it themselves.** The app's JSX compiles to `src/jsx` (`jsxImportSource` in `vite.config.ts` and `tsconfig.json`): React's runtime, except that string children and text attributes (`title`, `aria-label`, `aria-description`, `alt`, `placeholder`) of DOM elements go through `visible()`. Before this, views had to remember, and several did not (run argv and program, run errors, source health, config problems, toasts). Values in fields the user edits are left alone, as are props passed to components (the DOM element they end up in is cleaned). `visible()` is idempotent, so views that still call it are harmless. Measured with the storm and big-output browser tests: no difference in frame times. Text set outside JSX is not covered: the window title is built from counts only, and copy buttons copy the real value.
- The page is served with a Content-Security-Policy that allows no inline script or style and nothing from other origins ([0004](0004-daemon-and-config.md)); the real-daemon tests fail on any violation.
- Prompts are shown split: the rule's instruction, then the fenced untrusted data block in a separate, dashed box.
- Run output is not rendered as Markdown, even when an agent writes Markdown.

## Keyboard

One `keydown` listener and a registry (`app/keys.ts`). Views register their bindings while mounted; the help overlay (`?`) and the System view list whatever is registered, and buttons show their shortcut. Single keys are ignored while typing in a field; `mod+` combinations and Escape are not. While a modal dialog is open it owns the keyboard.

Global: `mod+k` palette, `?` help, `g f/u/r/a/s/p/y` views, `g i` next approval, `shift+p` pause or resume everything, `n` new rule, `shift+t` theme. Lists: `j`/`k`, Escape. Feed: `/` filter, space pause the tail, Home newest. Runs: `shift+a` approve, `shift+x` reject, `shift+c` cancel, `shift+r` rerun, End follow output. Rules: `e` edit, `shift+e` enable or disable, `mod+enter` test, `mod+s` save.

## Design

Calm and dense: 13 px type, 30 px rows, monospace for machine data, tabular numerals for times and counts. Colour means state and nothing else: green fired or healthy, amber needs you (approval, snoozed, degraded, paused), blue waiting or running, orange-red **loop protection**, red failed, grey blocked or off. Light and dark follow the system; System → Theme (or `shift+t`) overrides it per browser. No animation repaints continuously; the only transitions are 60 ms hover backgrounds, off under reduced motion. Long paths truncate in the middle and keep the file name; ids and paths have copy buttons.

## The mock daemon

`src/mock/` implements every method in `RpcMethods` (the `Handlers` type makes a missing one a compile error) over in-memory state, behind a socket that speaks JSON strings, so the app and the tests run the same client code path as with the real daemon. It routes events with a simplified version of `pulse_core::routing` (same order of checks, same outcomes and block reasons), runs scripted agents whose output looks like each adapter's, batches live events like the daemon does (at most 200 per batch every 250 ms, the rest counted as dropped, with outcomes on events and pages), omits default fields from definitions the way serde does, and sends `state.changed` with topics.

It seeds three hours of history: fires, debounces, every block reason the UI explains, a formatter whose own write is blocked by loop protection, a rule with a tripped circuit breaker, a snoozed rule, a disabled and an archived rule, runs in every status, a pending approval, a degraded and a failed source, project files in each trust state, and config errors. Controls (also in the palette in mock builds): an event storm, a run with megabytes of output, a supervised fire, dropping the connection, stopping and starting the daemon.

It is only imported when the build mode is `mock` (`pnpm dev:mock`, `pnpm build:mock`). `import.meta.env.MODE` is a build-time constant, so the production bundle contains neither the mock nor its controls.

Where the mock is simpler than the daemon: no dedupe, no `debounce_capacity`, no folding of file storms into `fs.burst`, no causation from working directories or tool calls (two scripted cases stand in: a formatter's rewrite, inferred, and the study agent's cards file, reported), the circuit breaker is a fixed 20 fires in 10 minutes, permission paths are compared with a JavaScript glob matcher, and run output is pushed per chunk instead of read from the store every 100 ms. Like the daemon, a debounce elapsing records a second delivery (fired) after the first (debounced). Treat the rest as guesses about the daemon, not as its specification; when the daemon turns out to differ, change the mock.

## Wiring it to the real daemon

The UI and the daemon were written at the same time without seeing each other. What disagreed, worst first, and where it was fixed:

1. **Login** (both). The UI expected `#token=<token>` and offered subprotocol `pulse.v1`, which the daemon does not select, so the browser dropped every connection; the daemon refused a bad token with an HTTP 401 that a browser cannot see. Now the login code flow above, subprotocol `pulse`, and 4401 for a browser upgrade with a bad token.
2. **Spent login codes worked again after a restart** (daemon): the single-use memory is in RAM. A daemon now refuses codes minted before it started.
3. **Outcomes per row** (contract): see "Delivery markers". Live batches and query pages carry outcomes; agents over MCP get the batch without them.
4. **Generated types lied about optional fields** (contract): `where`, `debounce_by`, `ignore`, `allowed_tools`, `disallowed_tools`, `extra_args`, `env` are omitted at their defaults but were typed as required, and `after` on run output params was required. A struct-level `ts(optional_fields)` overrides ts-rs's serde-based detection for non-`Option` fields; they are now marked, and a test compares the generated types with what serde emits.
5. **`~` in a project directory** (daemon): `projects.trust` refused `~/code/repo` (the UI's own placeholder) as not absolute. The dialog also showed the refusal in a toast hidden behind it; it now shows it in the dialog.
6. **`~` in filters** (daemon): `EventFilter.path` and `subject` were not expanded; `path:~/University` matched nothing.
7. **No home directory** (contract): `SystemStatus.user_home` added; paths show as `~/…`, with the absolute path in titles and copy buttons.
8. **Run counts went stale** (daemon): `state.changed` was sent only for topic changes. It is now also sent, with no topics, when run counts change. The sidebar still counts from the run index it keeps anyway; the status's counts are right for clients without one (the desktop shell's badge).
9. **Subscription response order** (daemon): output already stored could overtake the subscribe response. Subscriptions now deliver nothing until their response is queued. The client still tolerates the reverse.
10. **`rules.test` for a debounced rule** (UI): the daemon already returned `after_debounce`; the tester re-tested a copy without the debounce. It uses the field now.
11. **Config errors** (UI and contract doc): the notice said definitions from a broken file are not loaded; the daemon keeps the last good version of the user's file (a project file with errors is inactive).
12. **Batch size** (mock): 200, not 100.
13. **`runs.subscribe`** (mock): the mock reported a run once more when it left the filter's statuses; the daemon does not. The UI only subscribes with an empty filter.
14. **Storms of files** (mock): the real fs source folds more than 200 pending changes in one directory into one `fs.burst`; 10,000 unzipped files reach the UI as one event. The mock's storm is now ingested external events, which the daemon does deliver one by one.
15. Form fields had no accessible names (UI): each `Field` now gives its control an id.

Verified as assumed: `index.html` for paths without a file extension (`/runs/<id>`, `/rules/<name>`; names cannot contain dots); `events.query` cursors are exclusive in the direction of `order`; `runs.subscribe` with an empty filter delivers every change, latest state per 250 ms window (intermediate states can be skipped: views show state); run output ends with `done: true`, also for a finished run; dropped events are older than the batch.

Decided as is for v1:

- **Coalescing is a count.** `pulsecoalesced` says how many raw signals were folded, not which. The UI says so ("Pulse records how many, not which ones"). Keeping the signals would grow every event for little use.
- **Projects are not discovered.** Trusting takes a directory the user types (`~` allowed). Discovering `pulse.toml` files would mean scanning, or guessing from git sources; the user knows which repository they mean.
- **Inferred causation can block a legitimate event.** A new PDF that lands in `~/University` while a `lecture-notes` run (working directory `~/University`) is active used to be attributed to that run and blocked as a self-trigger. Since adapters report their agents' writes, that happens only when the run did something whose writes Pulse cannot see (0002, "Loop protection"). The UI shows the cause's kind on the event (`exact`, `reported`, `inferred`, from one table in `lib/explain.ts`), so a block on an inferred cause reads as the guess it is.

## Testing

- Unit (Vitest, `pnpm --dir apps/web test`): the RPC client (out-of-order responses, errors with data, timeouts, queued calls, no resend after a drop, backoff and reset, 4401, resubscribe with fresh params, early notifications, unsubscribe races, `state.changed`), the feed buffer (bounds, batching, gaps and their counts, merging history, disconnection gaps), filters (EventFilter mapping, URL round trip, query syntax), the rule and agent forms (round trips of every field and adapter, problems per field), formatting of untrusted text and prompts, and the mock's scenarios through the real client.
- Browser against the real daemon (Playwright, `pnpm --dir apps/web test:real`, after `pnpm build` and `cargo build -p pulse-cli`): each test starts the `pulse` binary on a temp home and port 0 with a real fs source and the fake agents. Login (spent, expired and missing codes; nothing stored holds the token), the live feed with outcomes, live run output, approve and reject, a rule written back to `pulse.toml` with hand edits and a broken file, recovery after a restart, a program that takes the port after the daemon stops (it gets proof requests and no token on reconnect; on reload its page reads storage and finds only a resume code the restarted daemon refuses), the CSP headers (and every test fails on a violation), and a hostile subject through a command agent checked for hidden characters in every view it reaches.
- Browser (Playwright, `pnpm --dir apps/web test:browser`): the production build with the mock compiled in. Storms, gap rows, holding the tail, a dropped connection, keyboard paths, the palette, the filter syntax, shortcut discovery, a draft dry run, creating a rule and taking it through every reverse state, untrusting and re-trusting a project, megabytes of output, structured output, and the measurements below. Tests wait for conditions, never for time.

## Measurements

Chromium 156 (headless shell) on the development machine (Linux arm64, 8 cores, shared with other builds), 1440×900, production build with the mock in the page. The mock's own work (routing, serializing frames) runs on the same main thread, so these numbers overstate what the UI costs with the real daemon.

Against the real daemon (debug build, same machine), feed open, measured in the page and from the page's own WebSocket frames:

| Case | What the daemon sent | Frames | Long tasks |
|---|---|---|---|
| 10,000 files unzipped into a watched folder | one `events.batch` with one `fs.burst` (2.3 KB) | p50 16.7 ms, p95 16.7 ms, max 33.3 ms | none |
| 10,000 events through `pulse ingest` (2.8 s) | 12 batches, 4 per second, at most 200 events and 53 KB each; 2,300 shown, 7,700 counted as dropped | p50 16.7 ms, p95 16.7 ms, max 16.8 ms | none |
| 30,000 events through `pulse ingest` (8.3 s) | 34 batches; 6,700 shown, 23,300 dropped | p50 16.7 ms, p95 16.7 ms, max 33.3 ms | none |

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
