# Agent fixtures

Tests never run a real agent (it costs money). They run these instead. Everything here is POSIX
`sh` plus `sleep`, `awk`, `head -c` and `tr`, so it works on Linux and macOS.

From another crate, the directory is `concat!(env!("CARGO_MANIFEST_DIR"), "/../pulse-agents/tests/fixtures")`.

## `fake-agent <mode> [args]`

A stand-in for any `command` agent. Use it as `command[0]`.

| Mode | Behaviour | Used for |
|---|---|---|
| `exit [CODE]` | `stdout line` on stdout, `stderr line` on stderr, exit `CODE` (0) | normal and non-zero exits |
| `args [ARG…]` | each argument on its own line | asserting argv as the process saw it |
| `stdin` | copies stdin to stdout | asserting the prompt or event JSON on stdin |
| `env` | the `PULSE_*` variables it received, sorted | the run environment contract |
| `replay FILE [DELAY]` | consumes stdin (into `$FAKE_AGENT_STDIN_FILE` if set), prints `FILE` line by line, sleeping `DELAY` seconds between lines, exits `$FAKE_AGENT_EXIT` (0) | live output, slow streams |
| `hang` | prints `ready`, then sleeps until killed | timeout, cancel, shutdown |
| `ignore-term` | prints `ready`; on each SIGTERM prints `term-ignored` and keeps running | the SIGKILL after the grace period |
| `grandchild [--ignore-term] [--detached] [--hang]` | starts a sleeping grandchild (ignoring SIGTERM; with its output on `/dev/null`), prints `grandchild <pid>`, then exits 0 or (`--hang`) sleeps until killed | orphans: the group must be gone afterwards |
| `escape` | starts a grandchild that leaves the group with `setsid` (Linux only) and keeps stdout open, prints `escaped <pid>`, then `partial` without newline, exits 0 | the documented limit: escapees survive, the run still ends |
| `write-file PATH [TEXT]` | writes `TEXT` to `PATH`, prints `wrote PATH` | loop protection (a run that causes an fs event) |
| `flood LINES [DONE_FILE]` | prints `LINES` numbered 100-byte lines, then creates `DONE_FILE` | backpressure (`DONE_FILE` cannot exist while nobody reads) |
| `long-line BYTES` | one line of `BYTES` × `x`, then `after` | the line-length cap |
| `bad-utf8` | invalid UTF-8, a CRLF line, a last line without newline | decoding |

Unknown modes exit 64.

**Parallel tests.** While a test spawns a process, the forked child briefly holds copies of
every pipe the test process has open, so another run's pipe can reach EOF a moment after its
agent exited. In real time that is nothing. On tokio's paused clock an idle runtime jumps
straight past the supervisor's grace period, and such a run ends with a spurious "pipe still
open" marker (output is still delivered). Tests that assert exact output should not supervise
processes concurrently in one test binary: `tests/supervisor.rs` serializes them with a mutex.

## `fake-claude`, `fake-codex`

Stand-ins for the `claude` and `codex` executables: set them as the agent's `program`. They print
their argv to stderr (`argv: …`), consume the prompt, and replay a recorded stream through
`fake-agent replay`. The agent's `env` chooses the stream (`PULSE_` names are reserved for
Pulse, hence `FAKE_AGENT_`):

| Variable | Default |
|---|---|
| `FAKE_AGENT_STREAM` | `claude-code/answer.jsonl` / `codex/answer.jsonl` |
| `FAKE_AGENT_DELAY` | none (seconds between lines) |
| `FAKE_AGENT_EXIT` | `0` (the real CLIs exit 1 for `bad-model`) |
| `FAKE_AGENT_STDIN_FILE` | none (where to save the prompt) |

## Recorded streams

Real output, recorded on 2026-11-16 with the cheapest model of each CLI in an empty temp
directory, then scrubbed (paths become `/tmp/pulse-run` or `/home/user`, session, message,
request and tool ids are replaced, thinking signatures and account rate-limit details removed,
the init event's tool, skill and plugin lists cut down).

| File | What happened |
|---|---|
| `claude-code/answer.jsonl` | Claude Code 2.1.325, haiku, `dontAsk`: one-sentence answer |
| `claude-code/tool-denied.jsonl` | reads `note.txt` (allowed), tries to write `out.txt` (denied by `dontAsk`), explains |
| `claude-code/bad-model.jsonl`, `.stderr` | unknown model: API error, exit 1 |
| `codex/answer.jsonl`, `.stderr` | codex-cli 0.169, `gpt-5.6-luna`, `read-only`, approvals `never`: one-sentence answer |
| `codex/sandbox-failed.jsonl` | the read-only sandbox could not start on the recording machine; the agent says so |
| `codex/bad-model.jsonl` | unknown model: `error` and `turn.failed`, exit 1 |
| `codex/tools.synthetic.jsonl` | **synthetic**, written from Codex's documented `exec --json` events: commands, a failed command, a file change, an MCP call, a web search, a plan. The sandbox did not work on the recording machine, so no real tool items exist. |
