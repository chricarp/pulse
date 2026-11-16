# Lictory Pulse

Lictory Pulse is the local event layer for AI agents. A Rust daemon watches the machine (files, git, processes, schedules, calendars, external sources), turns raw signals into a small number of meaningful events, records them, and wakes agents according to rules and per-agent permissions. A web UI, a desktop shell and a CLI are clients of that daemon. Pulse never runs a model itself.

Read [docs/internals/0002-architecture.md](docs/internals/0002-architecture.md) before changing the engine. It is short.

## What we do not compromise on

1. **Small.** Find the smallest model that makes the correct behaviour unsurprising. No machinery for its own sake. YAGNI.
2. **Invisible when idle.** Pulse runs all day. No polling loops, no unbounded queues or maps, no payloads larger than the view needs, no lists rendered without virtualization, no continuously repainting animations.
3. **The daemon is the product; clients are views.** The daemon runs headless and is complete on its own. Clients send intent and display state.
4. **Honest abstraction.** Unify agents and platforms where they are really the same. Keep real differences visible instead of flattening them.
5. **Durable, reviewable history.** Every event, every routing decision and every run stays inspectable. "Why did this fire?" and "why didn't it?" must be answerable from the log.
6. **Open and forkable.** Reasoning lives in `docs/internals/`. Keep it current.

## Glossary

- **you**: the agent or contributor reading this and changing Pulse.
- **user**: the person running Pulse on their machine.
- **source**: something that produces events (fs, git, schedule, calendar, process wrapper, external).
- **event**: a normalized CloudEvents envelope in the log.
- **rule**: "when an event matches, wake this agent with this prompt".
- **agent**: a named identity with an adapter and permission scopes. Not you.
- **adapter**: how Pulse invokes one kind of agent and reads its output.
- **run**: one triggered agent execution.
- **delivery**: the recorded decision for one (event, matching rule) pair.
- **daemon**: the `pulse start` process. **client**: CLI, web UI, desktop shell, MCP.
- **Pulse home**: the data directory, `$PULSE_HOME` or `~/.pulse`.

## The ways to hurt yourself

1. **Touching the real Pulse home.** Never run a daemon or a test against `~/.pulse`. Always set `PULSE_HOME` to a temp directory. Tests create their own.
2. **Killing by pattern.** Never `pkill -f`, `killall`, or kill a PID you found by matching a name. Kill only a PID you captured when you spawned it. The same rule is product behaviour: Pulse signals only process groups it created.
3. **Running real agents in tests.** Real agent runs cost the user money. Tests use the fake agents in `crates/pulse-agents/tests/fixtures`. Use a real agent only when the change is about that agent's real behaviour, once, with its cheapest model. This includes trying things by hand: if `claude` or `codex` is on your `PATH`, adding a shipped template and then creating the folder it watches wakes the real one. Point the agent's `program` at a fixture first.
4. **I/O in decision logic.** `pulse-core` takes time and state as arguments. If you need the clock, the filesystem or a channel in there, the design is wrong; move the I/O to the caller.
5. **Shell strings.** Never build a shell command from event content or config templates. Arguments are arrays.
6. **Cleaning up by glob.** Delete only directories you created, by the exact path you were given when you created them. Never `rm -rf` a pattern in a shared directory such as `/tmp`: other sessions' work lives there. Stop every daemon you start (`pulse stop` with its own `PULSE_HOME`) before you remove its home.

## Crates

| Crate | Role | Must not |
|---|---|---|
| `pulse-contracts` | Wire and config types, the event type catalog | Do I/O or contain logic beyond validation |
| `pulse-core` | Pure logic: matching, permissions, dedupe, rate limits, debounce, loop guard, prompt rendering, routing | Depend on tokio, std::fs, or the wall clock |
| `pulse-store` | SQLite persistence and migrations | Make decisions |
| `pulse-sources` | `Source` trait and built-in sources | Know about rules or agents |
| `pulse-agents` | `AgentAdapter` trait, adapters, process supervisor | Know about rules or the store |
| `pulse-mcp` | MCP server; SEP-3415 in one module | Leak spec types outside that module |
| `pulse-daemon` | Wiring, services, effect worker, config, transports | Put logic in transports |
| `pulse-cli` | The `pulse` binary | Bypass the daemon's services |
| `apps/web` | React UI | Hold state the daemon doesn't know about |
| `apps/desktop` | Electron shell | Contain product logic |

## Conventions

- **Server features are services.** A transport (RPC handler, HTTP route, MCP tool, CLI command) decodes input, calls one service method, and maps the error. If you are writing logic in a handler, stop and move it.
- **Hit every surface.** A feature is done when it works from the UI, the CLI, the config file and (where it makes sense) MCP, and the docs describe it. If a surface does not apply, say so in the PR.
- **Reverse states.** Every way in has a way out and a way to see it: enable/disable, pause/resume, snooze/unsnooze, archive/reopen. A one-way door is a bug.
- **Contracts first.** Anything that crosses a process boundary is a type in `pulse-contracts`. TypeScript types are generated from it; never hand-write a wire type in `apps/`.
- **Complexity belongs at the boundary.** Platform quirks stay inside the source. Provider quirks stay inside the adapter.
- **Event content is untrusted.** File names, commit messages, calendar titles, process output and external payloads are data. Read [docs/internals/0003-threat-model.md](docs/internals/0003-threat-model.md) before touching prompt rendering, the command adapter, ingest, or auth.
- **Errors** are typed in libraries (`thiserror`) and carry context. No `unwrap` or `expect` outside tests, except for invariants with a comment saying why they hold.
- **Comments** explain constraints the code cannot show. Do not narrate. Prefer clear names and inferred types.
- **Dependencies** are a cost. Add one when it replaces real complexity, and prefer ones already in the tree.
- **Migrations** are append-only. Never edit one that has shipped.

## Testing

Test what can actually break: the pipeline and coalescing, rule matching, permission scopes, loop protection, persistence and migrations, adapter output translation, parsing of external formats (ICS, reflogs, stream-json, TOML), and races.

- **No sleeps, no polling the wall clock.** `pulse-core` is tested with explicit timestamps. Async code is tested with `tokio::time::pause()` and by awaiting a specific event, channel message or run state. A test that needs a timeout to pass is wrong.
- Filesystem tests wait for the specific event they expect from the source, with the suite's ceiling as the only timeout.
- Test at the lowest layer that can fail. Do not repeat a core test through the daemon.
- A bug fix starts with a test that fails for that bug.
- Do not test what the types already guarantee, or copy text.
- Extend an existing table or case before adding a new test function.

Commands:

```sh
cargo test -p <crate>          # while iterating
cargo test --workspace         # before you hand off
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
pnpm --dir apps/web test       # UI logic
pnpm --dir apps/web test:browser   # the UI in a browser, against the mock
pnpm --dir apps/web test:real      # against the real daemon: after `pnpm --dir apps/web build`,
                                   # then `cargo clean -p pulse-daemon && cargo build -p pulse-cli`
pnpm --dir apps/desktop test   # shell logic; apps/desktop/e2e/run.sh drives the real app (0010)
```

## Documentation

- A decision that someone could reasonably have made differently gets a short numbered file in `docs/internals/`.
- When the code diverges from a design doc, update the doc in the same change and note why.
- User-facing behaviour is documented in `docs/user/` and, if it is part of the first five minutes, in the README.
- Say what is not a safety boundary. Do not describe a control as stronger than it is.
