# 0006: Agent adapters and the process supervisor

Status: v1, as built in `crates/pulse-agents`. Extends "Runs" and "Agent adapters" in
[0002](0002-architecture.md); the threat model is [0003](0003-threat-model.md).

## The shape

An adapter is pure: `invocation(agent config, run request) -> Invocation` (argv, env, cwd,
stdin) and a parser from native output lines to `RunOutput { kind, text, data }` and, for loop
protection, write facts. One supervisor does all process I/O for every adapter. Provider quirks
stay in the adapter's module; the supervisor knows nothing about Claude Code or Codex.

## Default flags, and what an unattended run does

Nobody can answer a permission prompt in a run triggered by a file appearing at 3 a.m. So each
adapter picks the most restrictive setting that **fails visibly instead of hanging**, and always
passes it explicitly, so the user's interactive defaults (which may be wide) never leak into
event-triggered runs.

| Adapter | Always passed | Default | When the agent wants more |
|---|---|---|---|
| `claude-code` | `--print --output-format=stream-json --verbose --permission-mode=… --permission-prompts=none` | `dontAsk` | The tool call is denied at once and the model is told. Output: an `error` for the `permission_denied` event, a failed `tool_result`, and after the `result` one more `error` naming every denied tool and pointing at `allowed_tools` / `permission_mode`. |
| `codex` | `exec --json --sandbox=… --config=approval_policy="…"` | `read-only`, `never` | Writes fail inside the sandbox, commands that would need approval fail, and the model is told. Output: a `tool_result` with the failure (`exit 1 (failed)`), then whatever the model says about it. |
| `command` | the configured argv | — | Pulse cannot know; the program decides. |

Why these:

- **`dontAsk`** is what Claude Code itself uses for its own scheduled tasks. It allows reads in
  the working directory and whatever `allowed_tools` or the user's settings pre-approve, and
  denies the rest without prompting (verified against the real CLI: see
  `tests/fixtures/claude-code/tool-denied.jsonl`). `--permission-prompts=none` additionally
  makes any other configured mode (`acceptEdits`, `manual`) deny instead of wait. It is left out
  when `extra_args` configure a prompt handler (`--permission-prompt-tool`).
- **`read-only` + `never`** for Codex: `exec` without them inherits `~/.codex/config.toml`,
  which is the user's interactive setup. Codex's git-repository check is not skipped; add
  `--skip-git-repo-check` to `extra_args` to run outside a repository.
- Nothing that skips checks (`bypassPermissions`, `--dangerously-*`, `danger-full-access`) is
  ever added by Pulse. A user can configure them; that is explicit config, not a default.
- Every configured value is attached to its flag with `=` (`--model=…`, `--allowedTools=…`),
  so a value that starts with `-` stays a value. The prompt, the only place event content
  appears, goes to stdin; event content never reaches the argv of these two adapters. Codex's
  `approval_policy` is spliced into a TOML expression, so it must be a bare word.

What these settings are **not**: a sandbox. Claude Code still applies the user's and the
working directory's settings files (allow rules, hooks) and Codex its config; a hostile
repository used as an agent's `cwd` brings its own `.claude/settings.json`. Pulse's scopes
decide what an agent is told; the agent runtime decides what it can do (see 0003).

## Output mapping

The tables are the module docs of `claude_code` and `codex`. The principles:

- A human follows a run live from `text`: assistant text, one-line tool calls
  (`Read /path`, `$ cargo test`), tool result previews (2000 characters), errors, and a
  `result` with the final answer. Cost, usage, session ids, tool inputs and the rest of the
  native event are in `data`, bounded (strings 4 KiB, 100 entries, 8 levels).
- Progress noise is dropped: Claude's `thinking_tokens` and allowed `rate_limit_event`,
  partial-message `stream_event`s, Codex's `turn.started` and `item.updated` (plans excepted).
  Empty thinking blocks (the model returned only a signature) are dropped too.
- **Parsers never fail.** A line that is not JSON, JSON of an unknown `type`, or a known type
  of an unexpected shape becomes raw `stdout` with the line unchanged; stderr is always raw
  `stderr`. CLIs change their formats; a new event shows up as raw output, not as a crash or a
  silent gap.
- Codex has no final-result event with text: `turn.completed` becomes `result` with the last
  agent message as its text and the usage in `data`.

## Write facts

What the agent's tools wrote is the best evidence loop protection has (0002, "Loop
protection"). A parser that can see it reports, per line, `WriteFact::Wrote(path)` (the path as
the agent gave it: untrusted text, absolute or relative) and `WriteFact::Opaque(what)` for
anything whose writes it cannot see; `AgentAdapter::reports_writes` says whether it can at all.
The supervisor sends a line's facts (`SupervisorEvent::Writes`) before its outputs.

| Adapter | Reports writes | Why |
|---|---|---|
| `claude-code` | yes | Tool calls are in the stream with their input; `Write`, `Edit`, `MultiEdit`, `NotebookEdit` name their file. The table of tools is the module doc of `claude_code`. |
| `codex` | no | `file_change` items list paths, but the only recorded example is synthetic (the sandbox did not run on the recording machine), and Codex reads files through shell commands, so nearly every run would be opaque from its first `cat`. Revisit with a real recording. |
| `command` | no | Pulse cannot see inside an arbitrary program. |

Classification is conservative: a tool is read-only only if it is on the adapter's list, a
write tool without its path is opaque, and so is any stdout line the parser does not
understand (a new event type, a line cut at `max_line_bytes`), because it could be a tool call
in a shape it does not know. A refused call counts; a subagent's start (`Task`) is opaque.

Verified once against the real CLI (Claude Code 2.1.325, haiku, `tests/fixtures/claude-code/
write.jsonl`): the tool call is printed 30 to 50 ms before the file is written; paths are
absolute; and with a working directory reached through a symlink, Claude Code reports (and
writes through) the canonical path, which is why the daemon also matches canonical spellings.

## The supervisor

### Lifecycle

Spawn directly (no shell) with `process_group(0)`, stdout and stderr piped, stdin from
`Invocation::stdin` or null. Inherited `PULSE_*` variables are removed first, so a run never
sees a stale `PULSE_EVENT_SUBJECT` or another run's id. With `SuperviseOptions::open_files`,
the child's soft descriptor limit is set between fork and exec (the daemon raised its own; see
[0007](0007-mcp-and-sep-3415.md), "Descriptors"). `Started { pid }` is sent before any output;
the pid is also the group id.

The run ends when the leader exits, the timeout elapses, or the cancellation token fires (the
daemon cancels all runs on shutdown). In every case the group gets SIGTERM, then SIGKILL after
`grace` unless every process holding the output pipes has exited by then. After a natural exit
this sweeps up whatever the agent left in its group (a dev server, an MCP server it forgot).
Then the leader is reaped, the pipes drained, the parser flushed.

### Signalling only our group

`killpg` on a reaped leader's id could hit an unrelated group that reused it. So the leader is
watched with `waitid(WNOWAIT)` on a blocking task, which reports the exit **without reaping**:
the zombie keeps its PID, so the group ID cannot be reused. Every signal is sent while the
child handle is still held, and the reap happens only after the last one. If something else in
the daemon reaps our child (a `waitpid(-1)`; tokio and std never do that), the supervisor sees
`ECHILD` and stops signalling.

### The orphan guarantee

**Guaranteed:** when `supervise` returns, the leader has been reaped and every process still in
its process group has been sent SIGKILL (so none of them runs user code again; their exit is
asynchronous by microseconds). This includes grandchildren that ignore SIGTERM, that the leader
abandoned, or that closed their output. When the future is dropped mid-run, the group gets
SIGKILL at once (no grace) and the leader is reaped on a short-lived thread.

**Not guaranteed:** a descendant that leaves the group, by `setsid()` (daemons, `setsid`,
double-forking servers, some terminal multiplexers) or `setpgid()`. Pulse does not track it and
will not kill it. If it still holds the output pipes, draining stops after `grace` without new
output (or after 1 MiB), whatever was already read is still delivered, and a `marker` says a
process outside the group holds the pipe, so the escape is visible in the run (tested with a
`setsid` grandchild on Linux).

**Stronger options**, not built:

- *Linux, cgroup v2*: one cgroup per run and `cgroup.kill` catches everything, `setsid`
  included. It needs a delegated cgroup subtree (a systemd user service with `Delegate=yes`, or
  `systemd-run --user --scope`), which Pulse cannot assume when started from a shell. Worth doing
  when the daemon ships as a systemd user unit.
- *Linux, `PR_SET_CHILD_SUBREAPER`* on the daemon: escaped descendants would be re-parented to
  it, so they could be found and killed. But it is process-wide, and the daemon would then have
  to reap every orphan itself without stealing tokio's children; more machinery than v1 needs.
  The tests use it (on the test process) to wait for killed grandchildren deterministically.
- *macOS*: no cgroups and no subreaper. Process groups are the practical limit; enumerating
  descendants (`proc_listchildpids`) would mean polling. Not worth it.

**Testing it.** The supervisor's tests run on tokio's paused clock. The leader is watched on a
blocking task, and tokio does not auto-advance while one runs, so a test advances time
explicitly while the agent is alive (timeouts, the SIGKILL after an ignored SIGTERM) and grace
periods elapse on their own once it has exited. One trap for other crates' tests: a forked
sibling briefly holds copies of every open pipe until it execs, which the paused clock turns
into a spurious "pipe still open" marker; see `tests/fixtures/README.md`.

### Backpressure, line bounds, timestamps

- The output channel is bounded. While it is full, the supervisor stops reading the pipes, the
  pipes fill, and the agent blocks on its next write: **a slow consumer slows the agent**;
  nothing is dropped and nothing grows. Timeout and cancellation still work while blocked, so a
  stalled consumer can make a run time out. In flight per run: two pipe buffers, two line
  buffers, the outputs of one line, and the channel.
- Lines are cut at `max_line_bytes` (default 1 MiB, because Claude Code puts a whole tool
  result on one line): the parser gets the first `max_line_bytes`, and a `marker` reports how
  many bytes were dropped. The rest of the line is discarded as it is read, so a multi-megabyte
  line costs at most one buffer.
- Bytes are decoded lossily (invalid UTF-8 becomes U+FFFD), `\r\n` is accepted, and a last line
  without a newline is still delivered.
- `SupervisorEvent::Output { at, output }`: `at` is when the line was read, not when the
  consumer got it, which can be later under backpressure. (Phase 1's signature was
  `Output(RunOutput)`, which could not carry it.)

## `command` adapter hardening

Per-argument rendering already keeps event content from adding arguments. On top of that, an
argument whose template *starts* with a placeholder may not render to something starting with
`-` unless an earlier argument is exactly `--`: a file named `--exec=sh` would otherwise be read
as an option. The run fails before spawning, with a message saying to add `--` or read
`PULSE_EVENT_FILE`. This turns 0003's honest limit into a refusal for the common case; values
after literal text (`--title={{subject}}`) are unaffected, since the option there is the user's.

## Adding an adapter

1. Add the settings struct and `AgentKind` variant in `pulse_contracts::config` (regenerate the
   TypeScript).
2. Add a module in `pulse-agents` with an `AgentAdapter` (pure: argv as an array, values
   attached to flags, prompt on stdin if the CLI allows it, the most restrictive non-hanging
   permission default passed explicitly) and an `OutputParser` (known events mapped, everything
   else raw; bounded `data`). Report write facts only if every tool call is visible in the
   output and its writes can be classified; otherwise leave `reports_writes` false.
3. Return it from `adapter_for`.
4. Record at most three tiny real runs with the cheapest model into `tests/fixtures/<kind>/`,
   scrub them, and test the parser against them, plus truncated and unknown lines. Add a
   `fake-<kind>` wrapper for daemon tests.
5. Add its row to the defaults table above.
