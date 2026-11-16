# v1 build plan

How v1 gets built, in what order, and what "done" means for each piece. Delete this file when v1 ships; the decisions live in the numbered documents.

## Order

The order is set by one constraint: parallel work needs stable interfaces. So the types and the pure logic come first, from one author, and everything else fans out against them.

### Phase 1: foundation (one author, serial)

- Cargo workspace, crate skeletons, MIT license, CI skeleton.
- `pulse-contracts`: event envelope and type catalog, config schema, run/delivery types, RPC methods, TypeScript generation.
- `pulse-core`: matcher, permission scopes, dedupe, rate limiter, debouncer, loop guard, prompt rendering, `route()`.
- `pulse-store`: schema, migrations, event log, deliveries, runs, output, outbox, retention.
- The `Source` and `AgentAdapter` traits, with a registry function in each crate that the daemon can call before the implementations exist.

Done when: `cargo test --workspace` passes, the core has table-driven tests for matching, scopes, loop policy and prompt rendering, and the store has migration and round-trip tests.

In parallel, independent of the code: research notes on the SEP-3415 draft.

### Phase 2: fan-out (parallel, one worktree each)

| Package | Contents | Done when |
|---|---|---|
| **daemon + CLI** | Engine loop, services, effect worker, config load/watch/write-back, project trust, RPC server, ingest, static UI serving, all CLI commands | The vertical slice works from the CLI with a fake source and a fake agent: event → log → rule → run → output. Loop protection and approvals are covered by tests. |
| **sources** | fs (with coalescing and storm folding), git (reflog-based), schedule, calendar (ICS file and URL), process wrapper, ingest parsing | Each source has tests against real temp directories and repositories; a 10k-file burst yields a bounded number of events. |
| **agents** | Supervisor (process groups, timeout, termination), `command`, `claude-code`, `codex` adapters | Parsers are tested against recorded output; the supervisor is tested for timeout, cancel and no orphans. |
| **mcp** | Tools, per-agent scoping, stdio bridge, SEP-3415 module behind a feature | An MCP client can list types, query and subscribe against a fake backend. |
| **web** | App shell, live feed (virtualized), rules (editor, test, history), runs (live output, approvals), agents, sources, command palette, keybindings | Works against a mock transport; the feed stays smooth with 100k rows. (Built: the feed holds at most about 25,000 rows, 5,000 live and 20,000 from the log, and is measured at that; see 0008.) |

### Phase 3: integration (parallel)

| Package | Contents |
|---|---|
| **integration** | Wire real sources, adapters and MCP into the daemon. End-to-end tests for every item in the definition of done. Demo rule templates. |
| **web against the real daemon** | Replace the mock, verify every view in a real browser, fix what the mock hid. |
| **desktop** | Electron shell: daemon lifecycle, window, tray, notifications. Packaging for Linux and macOS. One documented build command. |
| **docs** | README with the first five minutes, user guides, security model, roadmap. CI on Linux and macOS. |

### Phase 4: review

An independent review of the whole tree against this plan, the threat model and AGENTS.md. Fix what it finds.

## Definition of done

- A fresh macOS or Linux machine can build and launch the app with the documented steps.
- Dropping a PDF into a watched folder triggers the study agent, end to end, with streamed output in the UI.
- `pulse run -- <failing command>` triggers the diagnosis rule.
- A deliberately self-triggering rule is stopped by loop protection and shown as such in the UI.
- Unzipping a 10k-file archive into a watched folder neither freezes the UI nor floods agents.
- An MCP client can list event types, query history and subscribe.
- Pipeline, matching, permissions, persistence and adapters have meaningful tests with no sleeps.
- `docs/internals/` explains the key decisions and `AGENTS.md` exists.

## Known limits of how v1 is being built

- Development happens on Linux (arm64). macOS code paths are written against the `notify` abstraction and covered by CI on a macOS runner, but are not exercised on a developer machine before the first release. Say so in the README until that changes.
