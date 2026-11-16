# Lictory Pulse web UI

The React app the daemon serves at `/`. Design and reasoning: [docs/internals/0008-web-ui.md](../../docs/internals/0008-web-ui.md).

## Developing

Against a **real daemon** (what users get):

```sh
cargo build -p pulse-cli          # once, and after Rust changes
pnpm --dir apps/web dev:real
```

`dev:real` makes a temp `HOME` with `~/University` and `~/Inbox`, a Pulse home inside it, and a config whose agents are the fake agents from `crates/pulse-agents/tests/fixtures` (nothing real runs, nothing costs money). It starts the daemon on a free port and Vite proxying `/ws` and `/auth` to it, and prints a login URL for Vite's origin and a few `touch` commands that make things happen. Edits to the UI reload instantly. Ctrl-C stops both and removes the temp directory. It never touches `~/.pulse`.

With your own daemon instead: start it on a temp home, run `PULSE_DAEMON=http://127.0.0.1:<port> pnpm --dir apps/web dev`, and open the `pulse open --print` URL with the port replaced by Vite's (5173). Login codes work once, for two minutes.

Against the **mock daemon** (no Rust needed; seeded history, storms, megabytes of output, approvals, drops and restarts from the command palette):

```sh
pnpm --dir apps/web dev:mock
```

The mock follows the daemon's observed behaviour (batch sizes, outcomes on events, run subscriptions, `after_debounce`, `user_home`); where it is simpler, 0008 says so. When you find the daemon behaving differently from the mock, fix the mock.

## Checks

```sh
pnpm --dir apps/web typecheck
pnpm --dir apps/web lint
pnpm --dir apps/web test            # unit tests (Vitest)
pnpm --dir apps/web test:browser    # browser tests against the production build with the mock
pnpm --dir apps/web build && cargo build -p pulse-cli && pnpm --dir apps/web test:real
                                    # browser tests against the real daemon binary
```

The daemon embeds `apps/web/dist` when it is compiled, so rebuild `pulse-cli` after `pnpm build`; `test:real` refuses to run against a binary that embeds a different build. Each real test starts its own daemon on a temp home and port 0.

Wire types in `src/contracts/index.ts` are generated from `crates/pulse-contracts`; never edit them. Regenerate with `PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript`.
