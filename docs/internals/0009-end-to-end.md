# 0009: End to end

Status: v1, Phase 3 (integration), 2026-11-16. What running the whole product taught us: how the
end-to-end tests are built and how to add one, how loop protection behaves as actually observed
(including where it is wrong), and the measured cost of the daemon.

## The end-to-end tests

`crates/pulse-cli/tests/e2e/` is one test binary. Each test runs the **real `pulse` binary** as
the daemon (`pulse start --ready-line --port 0`) in a temp `PULSE_HOME` and a temp `HOME`, so
`~/University` in a shipped template is a temp directory. Sources are the real ones on real temp
directories and repositories; adapters and the supervisor are the real ones; models are the fake
agents from `crates/pulse-agents/tests/fixtures` (`fake-claude` stands in for `claude` through
the `claude-code` adapter's `program`). The CLI is the real binary too (`Env::run`), so `pulse
run`, `pulse ingest` and the agents' own `pulse run` / `pulse ingest` (with `PULSE_RUN_ID`) are
exercised as users run them.

| Module | What it proves |
|---|---|
| `templates` | Both shipped templates as shipped (only durations shortened and `program` added): a PDF runs the study agent with output reaching a subscriber while the run is running; a second PDF saved during that run gets its own run, and the agent's summary is attributed `reported`; `pulse run -- <failing>` runs the diagnosis and a passing command does not |
| `loops` | Self-trigger by inference (written at once, through a debounced rule, by a process that left the group and writes after the run ended), by a reported `Write` (in the agent's `cwd` and outside it), inference again after a `Bash` call, an unreported write taken for the user's, exact causation via `pulse run` and `pulse ingest`, the chain-depth limit with two rules that trigger each other, the circuit breaker when attribution fails, a replay |
| `restart` | A crash with runs running and queued and limits in force; a graceful stop; an agent orphaned by a crash that writes after the restart |
| `storm` | 10,000 files unzipped (events, runs, live batches; `system.status` calls are answered while the storm is still being processed, checked by the order of observations; latency is printed, not asserted, because it measures the machine); a real `git clone` and `git checkout` in a folder with an fs and a git source |
| `sources` | A git commit and branch switch, a schedule and a calendar event fire rules |
| `hostile` | Instruction-like, option-like, control-character, non-UTF-8 and 255-byte names; a 1 MB commit message; a symlink out of scope; an external payload claiming a built-in type |
| `cli` | Exit codes without a daemon, `pulse run` exit codes and signals, `init`, adding and removing definitions, the `--json` shapes, the `--json` contract for every command with and without a daemon (a table that fails when `pulse help` lists a command it lacks), `--json` streams with hostile content, `tail` showing an event before its run |

**How they wait.** `support::Watch` subscribes to every event and run on one connection and
accumulates what the daemon pushes; `Watch::until(what, predicate)` re-checks the predicate after
every notification, with `CEILING` (60 s) as the only timeout, which is reached only when a test
fails. Helpers on top: `run_in`, `run_status`, `event`, `delivery` (deliveries are committed with
their event, so the log is queried once the event is seen), `output_line`, `sources`. Two rules
learnt the hard way:

- **Read before you wait.** A source can fire within milliseconds of startup, and a fast run can
  finish before the test subscribes; `run_in` and `run_status` read the log first, then wait.
- **Agents that write print the tool call first.** `fake-claude` replays a stream, and a line
  `#write PATH` in it makes `fake-agent` write the file after printing the line before, as
  Claude Code prints a call and then runs it (`support::claude` builds the lines).
- **Agents that must wait for the test block on a fifo** (`support::Fifo`), never on a sleep:
  the agent makes a fifo named after its run, prints `ready`, and reads it; the test waits for
  `ready` in the run's output and writes a line when it wants the agent to go on. The fifo's
  blocking `open` runs on a plain thread, not tokio's blocking pool: a pool thread stuck in
  `open` (the agent died) keeps the runtime, and so the test binary, from ever exiting.

Durations that have to pass are configured short (`quiet = "100ms"`, `debounce = "200ms"`). Where
time itself is the subject (the schedule), the test waits for the event that time produces. A
failing test keeps its temp directory and prints its path (config, `logs/pulse.log`, the
database), which is how the flakes below were diagnosed.

**Adding one.** Write the config the test needs (start from a template with `support::template`
and `replace`, which fails if the template text changed), `env.start()`, `daemon.watch()`,
`watch.sources(SourceState::Healthy)` if a source must be watching first, cause the thing (write
a file, `env.run(&["run", "--", …])`), then wait for the specific event, delivery or run state.
Finish with `daemon.stop()`; every process the harness starts is also killed by its captured PID
when its handle drops. Run `cargo test -p pulse-cli --test e2e` several times, under load too
(`cargo test --workspace` in parallel), before trusting it.

## Loop protection, as observed

Every way we tried to get a loop through, with an agent that really writes. "Command agent" is
the `command` adapter (opaque: inference); "Claude agent" is `fake-claude` through the
`claude-code` adapter (reported writes):

| Attempt | Result |
|---|---|
| Claude agent's `Write` into the folder its rule watches, cwd = that folder | Blocked, `loop_self_trigger` (reported) |
| The same with the agent's cwd elsewhere (its per-run directory) | Blocked (reported). Under inference alone this was not attributed and looped until the circuit breaker |
| Claude agent calls `Bash`, then a file appears in its cwd (written without a tool call) | Inferred, blocked; the run's output has a marker saying it is now opaque and why |
| Claude agent writes a file without printing the call (stands in for a fact that arrives late) | Taken for the user's: the rule fires once more, as a new chain (depth 1); that run's rewrite is a `modified` and matches nothing |
| The user saves a second PDF while a `lecture-notes` run reads the first | Was **skipped** as a self-trigger (inferred). Now fires its own run: the run is observed (only `Read` so far), so the PDF is not its |
| Command agent writes into the folder its rule watches, at once, cwd = that folder | Blocked, `loop_self_trigger` (inferred). Before the fix in the worker, a write in the first milliseconds could predate the run's recorded start and **fire again** under load |
| Same, through a debounced rule | Blocked before the debounce (the loop guard precedes it) |
| A process that left the run's group (`setsid`) and writes after the run ended | Blocked while within `causation_grace` (30 s) of the run's end |
| The same writing after the grace period | Not attributed: fires again. Bounded only by cooldown, rate limit and the circuit breaker |
| Command agent with its cwd elsewhere, writing into the watched folder by absolute path | Not attributed: loops until the circuit breaker opens (3 runs with `max_fires = 3`; 20 per 10 minutes by default, then the rule is held for an hour), recorded as `circuit_open` on the delivery and on the rule, cleared by `unsnooze` |
| `pulse run` / `pulse ingest` from inside the run | Exact causation through `PULSE_RUN_ID`; blocked |
| Two rules that trigger each other (exact) | Runs at depth 1, 2, 3; the fourth is `loop_chain_depth` |
| Replaying the blocked event | Fires once (a replay drops causation: it is the user's request); that run's own write is blocked in turn. Not a loop |
| Daemon crash while the agent runs, agent writes after the restart | Was **not attributed** (attribution was in memory only) and re-fired the rule. Fixed: runs are reloaded at startup (0004) |
| An external source retrying a keyed event after a restart | Was **accepted twice** (dedupe was in memory only). Fixed |

Limits that remain, stated plainly:

- **Inference still decides for opaque runs, with both of its errors.** A `command` or `codex`
  run, or a Claude Code run after its first shell command, MCP tool, subagent or unrecognized
  output line, claims everything that appears in its `cwd` until 30 s after it ends: in
  `lecture-notes`, if the model tries `Bash` (`pdftotext`, say; refused, but the attempt counts),
  a PDF the user saves during that run is skipped as before, with the `pulse replay` hint in
  `pulse rules history`. And such a run's writes outside its `cwd` that it did not report are
  not attributed at all; only cooldown, rate limit and the breaker stop those (with the
  defaults: up to 20 runs in 10 minutes, then one hour held, repeating).
- **A late fact makes the agent's write the user's.** If the tool call reaches the engine more
  than the fs quiet period after the write (the agent buffers its output; the store stalls with
  more than 256 outputs queued behind it), the write is not attributed and, if it matches the
  run's own rule, fires it once more as a new chain, so chain depth does not bound it. Not
  observed; the tool call led the write by 30 to 50 ms against a 2 s default quiet period. A
  rule with `quiet = "100ms"` has much less margin.
- **Writes that are not in the stream are the user's.** Hooks from the user's or the project's
  Claude Code settings that print nothing (a formatter after `Write` usually rewrites the same
  file, which is covered), and Claude Code's own files under `~/.claude` if a source watches
  them.
- **A reported path can be spelled differently from the event.** The daemon matches the path as
  given, its canonical form and the canonical form spelled under the agent's configured `cwd`.
  A write through a symlink that the fs source knows by yet another name (a source configured
  through a symlink that is not the agent's `cwd`) does not match: taken for the user's.
- **Reported over-attributes a little.** A path the agent named is its own until 30 s after the
  run, even if the write was refused: the user's change to exactly that file in that window is
  skipped. An `fs.burst` whose sample contains one reported path is attributed whole.
- After a crash, an orphaned run counts as active and opaque until its timeout (default 15
  minutes): its own folder's events in that window are attributed to it, false positives
  included.
- The grace period still catches the user after opaque runs: a commit made in `~/code` a second
  after a `build-failure` diagnosis that ran a command was attributed to that run (chain depth
  2). A diagnosis that only read files is observed, and the commit is the user's.
- Process exits (`pulse run`) are never inferred, only exact: an agent whose sandbox strips
  `PULSE_RUN_ID` from its tools' environment has its `pulse run` reports unattributed.

## Measurements

Release build (`[profile.release]`: fat LTO, one codegen unit, stripped; `panic` stays
`unwind`), Linux arm64, on a shared and busy host, 2026-11-16. The daemon ran the
`lecture-notes` template (with `fake-claude`) on a tree of 3,031 directories. Method: start
`pulse start --ready-line`, read `utime + stime`, `VmRSS`, `VmHWM`, the fd count and the
`inotify` lines of `/proc/<pid>/fdinfo/*`, unzip 10,000 files while timing `pulse --json status`
in a loop, then `pulse stop` with a run of `fake-agent hang` in flight.

| What | Measured |
|---|---|
| Binary | 15.7 MB (13.1 MB before the MCP server was merged; 17.2 MB with the default profile) |
| Startup | 30 ms to listening; 312 ms until the fs source on 3,031 directories is healthy |
| Idle CPU | **0** clock ticks over 300 s (`utime + stime` unchanged) |
| Idle memory | 17.3 MB RSS, 13 threads, 23 fds, 3,032 inotify watches (one per directory, plus the config directory) |
| 10k-file unzip | One `fs.burst` (`created: 10000`), 0 runs; 1.8 s CPU in total; peak RSS (`VmHWM`) 17.9 MB; +101 inotify watches for the new directories |
| `system.status` during the unzip | From a separate `pulse --json status` process each time: 691 calls, median 8.4 ms, max 23 ms (process start included). In-process client, debug build (e2e test): slowest 8 ms over ~1,000 calls |
| Database after the unzip | 165 KB (94 KB after the checkpoint at stop) |
| Stop with a run in flight | 17.5 ms (`pulse stop` returns after the run's group is terminated and the daemon exited) |
| 100 config reloads switching the watched tree | fds 23 → 23, threads 13 → 13, inotify watches 102 → 102, RSS 39 → 41.5 MB (debug build) |

**Retention.** With `events_max_count = 1000` and 1,000 events ingested between restarts, every
start pruned back to 1,000, and the database file plateaued (about 2.4 MB after pruning, 4.3 MB
at the peak): SQLite reuses the freed pages, the file does not shrink. Retention runs at start and
hourly, so the database can exceed its limits by an hour's worth of events. It used to log only the
last batch's count (usually zero), so the log never showed it running; it now logs totals.

**Logs** rotate at 10 MB, five old files (unit-tested in `logging`). At `info` the daemon writes
almost nothing: 167 bytes over the whole measurement above.

## Storms and the quiet period

Storm folding needs `burst_threshold` (200) changes **pending at once**. With the default quiet
period (2 s) a clone of this repository (about 300 files) folds, and a checkout right after it
folds into their net effect. With `quiet = "200ms"` on a loaded machine the same clone settled
file by file before 200 were pending: 326 creations and 323 deletions, and the agent's runs were
capped at 12 by its backlog and the circuit breaker. A slow trickle of many files (a large
archive on a slow disk) is likewise reported file by file: bounded by the backlog and the breaker,
but not small. Rules on broad folders should `debounce`.

## Known flakes found and fixed while writing the tests

- The worker's run start time (above) — a product bug.
- A test waited for a run by notification only and missed one that ended before it subscribed
  (the calendar source fires within milliseconds of startup when an event is already due).
- The "escaped writer" agent could be killed with its group before `setsid` took effect; it now
  waits for its child to leave the group (a fifo handshake), as `fake-agent escape` does.

## One real run

To check the real thing once: the `lecture-notes` template with its folder replaced by a temp
directory and `model = "haiku"`, a generated one-page PDF, the real `claude` (2.1.325). Pulse ran
`claude --print --output-format=stream-json --verbose --permission-mode=dontAsk
--permission-prompts=none --model=haiku --allowedTools=Read --allowedTools=Write` in the folder,
with the prompt on stdin; output streamed into the run (`Read` of the PDF, two `Write`s, the
answer, the result); it succeeded in 13.5 s, 4 turns, $0.0058 reported, and wrote
`lecture-07.summary.md` and `lecture-07.flashcards.md` (15 cards). Its two files were attributed
to the run (inferred) and matched no rule.

The template pre-approved a bare `Write`, which allows writing anywhere (a PDF that talked the
model into it could write `~/.bashrc`). It now pre-approves `Edit(./**)` (relative to the
agent's `cwd`); a second real run wrote both files with no denials ($0.0053). That a write
*outside* the folder is now refused follows from `dontAsk` refusing whatever is not
pre-approved (0006), but was not run.
